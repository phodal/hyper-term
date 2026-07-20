use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hyper_term_core::{SandboxCompileRequest, SandboxLaunchPlan, SandboxLauncher};
use hyper_term_protocol::{
    Actor, OperationId, SandboxEnforcement, SandboxEnvironmentPolicy, SandboxFileSystemPolicy,
    SandboxLifetime, SandboxNetworkPolicy, SandboxPathAccess, SandboxPathRule,
    SandboxProcessPolicy, SandboxProfile, SandboxResourceLimits, TerminalCommand,
};
use hyper_term_sandbox::MacOsSeatbeltLauncher;
use uuid::Uuid;

use crate::DriverError;

/// Rust-selected containment inputs for one structured Agent process tree.
///
/// The credentialed proxy URL is deliberately separate from the serializable
/// policy. It is bound into the exact command digest and injected only into the
/// launched child environment.
#[derive(Clone, Debug)]
pub struct AgentContainmentConfig {
    pub proxy_url: String,
    pub credentialed_proxy_url: String,
    pub allowed_hosts: Vec<String>,
    pub allowed_unix_sockets: Vec<PathBuf>,
    pub read_paths: Vec<PathBuf>,
    pub write_paths: Vec<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_agent_task_sandbox(
    driver_id: Uuid,
    executable: &Path,
    arguments: &[OsString],
    working_directory: &Path,
    command_environment: &BTreeMap<String, OsString>,
    authority_environment: &BTreeMap<String, OsString>,
    proxy_url: &str,
    allowed_hosts: &[String],
    allowed_unix_sockets: &[PathBuf],
    read_paths: impl IntoIterator<Item = PathBuf>,
    write_paths: impl IntoIterator<Item = PathBuf>,
) -> Result<SandboxLaunchPlan, DriverError> {
    let profile = agent_task_sandbox_profile(
        executable,
        working_directory,
        authority_environment,
        proxy_url,
        allowed_hosts,
        allowed_unix_sockets,
        read_paths,
        write_paths,
    )?;
    compile_agent_task_sandbox_from_profile(
        driver_id,
        executable,
        arguments,
        working_directory,
        command_environment,
        profile,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_task_sandbox_profile(
    executable: &Path,
    working_directory: &Path,
    authority_environment: &BTreeMap<String, OsString>,
    proxy_url: &str,
    allowed_hosts: &[String],
    allowed_unix_sockets: &[PathBuf],
    read_paths: impl IntoIterator<Item = PathBuf>,
    write_paths: impl IntoIterator<Item = PathBuf>,
) -> Result<SandboxProfile, DriverError> {
    let mut rules = ["/System", "/usr", "/bin", "/sbin", "/Library"]
        .into_iter()
        .map(|path| SandboxPathRule {
            path: PathBuf::from(path),
            access: SandboxPathAccess::Read,
        })
        .collect::<Vec<_>>();
    rules.push(SandboxPathRule {
        path: executable.to_path_buf(),
        access: SandboxPathAccess::Read,
    });
    rules.push(SandboxPathRule {
        path: working_directory.to_path_buf(),
        access: SandboxPathAccess::Read,
    });
    rules.extend(read_paths.into_iter().map(|path| SandboxPathRule {
        path,
        access: SandboxPathAccess::Read,
    }));
    rules.extend(write_paths.into_iter().map(|path| SandboxPathRule {
        path,
        access: SandboxPathAccess::Write,
    }));
    Ok(SandboxProfile {
        enforcement: SandboxEnforcement::Native,
        filesystem: SandboxFileSystemPolicy { rules },
        network: SandboxNetworkPolicy::ProxyOnly {
            proxy_url: proxy_url.to_owned(),
            allowed_hosts: allowed_hosts.to_vec(),
            allowed_unix_sockets: allowed_unix_sockets.to_vec(),
        },
        environment: SandboxEnvironmentPolicy {
            clear_inherited: true,
            variables: utf8_environment(authority_environment)?,
        },
        process: SandboxProcessPolicy {
            allow_child_processes: true,
            allow_any_executable: true,
            allowed_executables: Vec::new(),
        },
        resources: SandboxResourceLimits::default(),
        lifetime: SandboxLifetime::OneTask,
    })
}

pub(crate) fn compile_agent_task_sandbox_from_profile(
    driver_id: Uuid,
    executable: &Path,
    arguments: &[OsString],
    working_directory: &Path,
    command_environment: &BTreeMap<String, OsString>,
    profile: SandboxProfile,
) -> Result<SandboxLaunchPlan, DriverError> {
    let command = TerminalCommand {
        program: utf8_path(executable, "Agent executable")?,
        args: arguments
            .iter()
            .map(|argument| utf8_os(argument, "Agent argument"))
            .collect::<Result<Vec<_>, _>>()?,
        cwd: Some(working_directory.to_path_buf()),
        env: utf8_environment(command_environment)?,
    };
    MacOsSeatbeltLauncher
        .compile(&SandboxCompileRequest {
            operation_id: OperationId::from_uuid(driver_id),
            operation_revision: 1,
            actor: Actor::System,
            command,
            profile,
        })
        .map_err(|error| DriverError::InvalidContainment(error.to_string()))
}

pub(crate) fn apply_managed_proxy_environment(
    environment: &mut BTreeMap<String, OsString>,
    credentialed_proxy_url: &str,
) {
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "WS_PROXY", "WSS_PROXY"] {
        environment.insert(name.into(), credentialed_proxy_url.into());
    }
    environment.insert("NO_PROXY".into(), OsString::new());
    environment.insert("NODE_USE_ENV_PROXY".into(), "1".into());
}

fn utf8_environment(
    environment: &BTreeMap<String, OsString>,
) -> Result<BTreeMap<String, String>, DriverError> {
    environment
        .iter()
        .map(|(name, value)| {
            utf8_os(value, "Agent environment value").map(|value| (name.clone(), value))
        })
        .collect()
}

fn utf8_path(path: &Path, label: &str) -> Result<String, DriverError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| DriverError::InvalidContainment(format!("{label} path is not UTF-8")))
}

fn utf8_os(value: &OsString, label: &str) -> Result<String, DriverError> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| DriverError::InvalidContainment(format!("{label} is not UTF-8")))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn proxy_credentials_bind_the_action_without_entering_the_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let runtime = temporary.path().join("runtime");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let executable = PathBuf::from("/usr/bin/true").canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let credentialed = endpoint.replacen("http://", "http://hyper-term:secret-token@", 1);
        let authority_environment =
            BTreeMap::from([("HOME".into(), runtime.clone().into_os_string())]);
        let mut command_environment = authority_environment.clone();
        command_environment.insert("HTTPS_PROXY".into(), credentialed.clone().into());
        let plan = compile_agent_task_sandbox(
            Uuid::new_v4(),
            &executable,
            &[],
            &workspace,
            &command_environment,
            &authority_environment,
            &endpoint,
            &["api.openai.com".into()],
            &[],
            [],
            [runtime],
        )
        .unwrap();

        let serialized_profile = serde_json::to_string(&plan.compiled.profile).unwrap();
        assert!(!serialized_profile.contains("secret-token"));
        assert_eq!(plan.command.env.get("HTTPS_PROXY"), Some(&credentialed));
    }
}
