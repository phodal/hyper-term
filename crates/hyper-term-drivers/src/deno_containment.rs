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

pub(crate) fn compile_deno_task_sandbox(
    driver_id: Uuid,
    executable: &Path,
    arguments: &[OsString],
    working_directory: &Path,
    environment: &BTreeMap<String, OsString>,
    read_paths: impl IntoIterator<Item = PathBuf>,
    write_paths: impl IntoIterator<Item = PathBuf>,
) -> Result<SandboxLaunchPlan, DriverError> {
    let command = TerminalCommand {
        program: utf8_path(executable, "Deno executable")?,
        args: arguments
            .iter()
            .map(|argument| utf8_os(argument, "Deno argument"))
            .collect::<Result<Vec<_>, _>>()?,
        cwd: Some(working_directory.to_path_buf()),
        env: environment
            .iter()
            .map(|(name, value)| {
                utf8_os(value, "Deno environment value").map(|value| (name.clone(), value))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
    };
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
    rules.extend(read_paths.into_iter().map(|path| SandboxPathRule {
        path,
        access: SandboxPathAccess::Read,
    }));
    rules.extend(write_paths.into_iter().map(|path| SandboxPathRule {
        path,
        access: SandboxPathAccess::Write,
    }));
    let profile = SandboxProfile {
        enforcement: SandboxEnforcement::Native,
        filesystem: SandboxFileSystemPolicy { rules },
        network: SandboxNetworkPolicy::Offline,
        environment: SandboxEnvironmentPolicy {
            clear_inherited: true,
            variables: command.env.clone(),
        },
        platform: Default::default(),
        process: SandboxProcessPolicy {
            allow_child_processes: false,
            allow_any_executable: false,
            allowed_executables: Vec::new(),
        },
        resources: SandboxResourceLimits::default(),
        lifetime: SandboxLifetime::OneTask,
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
    use std::time::Duration;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES, DriverEvent, DriverFraming, DriverKind,
        DriverManifest, DriverProcess, DriverSpec, DriverState, sandbox_permission_profile,
        sha256_file,
    };

    #[test]
    #[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
    fn real_deno_cannot_read_host_connect_loopback_or_spawn_a_child() {
        let executable =
            PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
                .canonicalize()
                .unwrap();
        let expected_digest =
            std::env::var("HYPER_TERM_DENO_SHA256").expect("HYPER_TERM_DENO_SHA256");
        assert_eq!(sha256_file(&executable).unwrap(), expected_digest);

        let root = tempdir().unwrap();
        let scratch = root.path().join("scratch");
        let cache = root.path().join("cache");
        let secret = root.path().join("host-secret.txt");
        let child_target = scratch.join("child-created.txt");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(&secret, "must stay outside Deno").unwrap();
        let scratch = scratch.canonicalize().unwrap();
        let cache = cache.canonicalize().unwrap();
        let secret = secret.canonicalize().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let script = format!(
            r#"const result = {{}};
try {{ await Deno.readTextFile({secret}); result.host_read = "allowed"; }} catch {{ result.host_read = "denied"; }}
try {{ await fetch({url}); result.network = "allowed"; }} catch {{ result.network = "denied"; }}
try {{ await new Deno.Command("/usr/bin/touch", {{ args: [{target}] }}).output(); result.child = "allowed"; }} catch {{ result.child = "denied"; }}
console.log(JSON.stringify(result));"#,
            secret = serde_json::to_string(&secret.to_string_lossy()).unwrap(),
            url = serde_json::to_string(&url).unwrap(),
            target = serde_json::to_string(&child_target.to_string_lossy()).unwrap(),
        );
        let arguments = vec![OsString::from("eval"), OsString::from(script)];
        let environment = BTreeMap::from([
            ("DENO_DIR".into(), cache.clone().into_os_string()),
            ("DENO_NO_PROMPT".into(), OsString::from("1")),
            ("DENO_NO_UPDATE_CHECK".into(), OsString::from("1")),
            ("HOME".into(), scratch.clone().into_os_string()),
            ("NO_COLOR".into(), OsString::from("1")),
            ("TMPDIR".into(), scratch.clone().into_os_string()),
        ]);
        let driver_id = Uuid::new_v4();
        let sandbox = compile_deno_task_sandbox(
            driver_id,
            &executable,
            &arguments,
            &scratch,
            &environment,
            Vec::new(),
            [cache, scratch.clone()],
        )
        .unwrap();
        let permission_profile = sandbox_permission_profile(&sandbox);
        let process = DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id,
                kind: DriverKind::DenoGenUi,
                implementation_version: "2.9.3".into(),
                protocol_version: "containment-probe-v1".into(),
                capabilities: Vec::new(),
                transport: "stdio-json-lines".into(),
                executable_sha256: expected_digest,
                permission_profile,
            },
            executable,
            arguments,
            working_directory: scratch,
            environment,
            sandbox: Some(sandbox),
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 4096,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })
        .unwrap();
        let DriverEvent::Message { payload, .. } =
            process.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("Deno containment probe did not emit its JSON result")
        };
        assert_eq!(payload["host_read"], Value::String("denied".into()));
        assert_eq!(payload["network"], Value::String("denied".into()));
        assert_eq!(payload["child"], Value::String("denied".into()));
        assert!(!child_target.exists());
        assert!(listener.accept().is_err());
        assert!(matches!(
            process.recv_timeout(Duration::from_secs(5)).unwrap(),
            DriverEvent::Exited {
                state: DriverState::Closed,
                ..
            }
        ));
    }
}
