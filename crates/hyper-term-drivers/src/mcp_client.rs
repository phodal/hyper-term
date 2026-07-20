use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use hyper_term_core::sandbox_profile_digest;
use hyper_term_protocol::{
    EXECUTION_CONTEXT_SCHEMA_VERSION, ExecutionMode, LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
    LocalMcpCredentialScope, LocalMcpServerLaunch, LocalMcpServerLifecycle, McpArgumentsDigest,
    McpRuntimeIdentityDigest, ResolvedExecutionContext, SandboxLifetime, SandboxProfile,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::sha256_file;

const MAX_SERVER_ID_BYTES: usize = 64;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct LocalMcpServerConfig {
    pub server_id: String,
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub execution_context: ResolvedExecutionContext,
    pub roots_snapshot_sha256: Option<String>,
    pub lifecycle: LocalMcpServerLifecycle,
    pub credential_scope: LocalMcpCredentialScope,
}

#[derive(Clone)]
pub struct PreparedLocalMcpServer {
    pub launch: LocalMcpServerLaunch,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<String, OsString>,
    pub sandbox: SandboxProfile,
}

pub fn prepare_local_mcp_server(
    config: LocalMcpServerConfig,
) -> Result<PreparedLocalMcpServer, LocalMcpPlanError> {
    validate_server_id(&config.server_id)?;
    if config.execution_context.schema_version != EXECUTION_CONTEXT_SCHEMA_VERSION
        || config.execution_context.mode == ExecutionMode::User
        || !config.execution_context.environment.clear_inherited
    {
        return Err(LocalMcpPlanError::UnsafeExecutionContext);
    }

    let executable = canonical_exact_file(&config.executable, "MCP executable")?;
    let actual_sha256 = sha256_file(&executable)?;
    if actual_sha256 != config.executable_sha256 {
        return Err(LocalMcpPlanError::ExecutableDigestMismatch {
            expected: config.executable_sha256,
            actual: actual_sha256,
        });
    }
    let working_directory = canonical_exact_directory(&config.working_directory)?;
    let context_working_directory = config
        .execution_context
        .workspace
        .working_directory
        .canonicalize()
        .map_err(|error| LocalMcpPlanError::Path {
            label: "execution-context working directory",
            message: error.to_string(),
        })?;
    if working_directory != context_working_directory {
        return Err(LocalMcpPlanError::WorkingDirectoryMismatch);
    }

    let sandbox = config
        .execution_context
        .requested_sandbox
        .clone()
        .ok_or(LocalMcpPlanError::UnsafeExecutionContext)?;
    if sandbox.lifetime != SandboxLifetime::OneTask
        || !sandbox.environment.clear_inherited
        || sandbox.environment.variables != config.execution_context.environment.variables
    {
        return Err(LocalMcpPlanError::UnsafeSandbox);
    }
    let sandbox_profile_digest = sandbox_profile_digest(&sandbox)
        .map_err(|error| LocalMcpPlanError::Sandbox(error.to_string()))?;
    validate_optional_sha256(config.roots_snapshot_sha256.as_deref())?;

    let arguments = validate_arguments(&config.arguments)?;
    let arguments_digest = McpArgumentsDigest::parse(sha256_json(&arguments)?)
        .map_err(|error| LocalMcpPlanError::Digest(error.to_string()))?;
    let argument_count =
        u16::try_from(arguments.len()).map_err(|_| LocalMcpPlanError::TooManyArguments)?;
    let identity = LocalMcpRuntimeIdentityInput {
        schema_version: LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
        server_id: &config.server_id,
        executable: &executable,
        executable_sha256: &actual_sha256,
        arguments_digest: arguments_digest.as_str(),
        argument_count,
        working_directory: &working_directory,
        context_digest: config.execution_context.context_digest.as_str(),
        sandbox_profile_digest: sandbox_profile_digest.as_str(),
        roots_snapshot_sha256: config.roots_snapshot_sha256.as_deref(),
        lifecycle: config.lifecycle,
        credential_scope: config.credential_scope,
    };
    let runtime_identity_digest = McpRuntimeIdentityDigest::parse(sha256_json(&identity)?)
        .map_err(|error| LocalMcpPlanError::Digest(error.to_string()))?;
    let launch = LocalMcpServerLaunch {
        schema_version: LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
        server_id: config.server_id,
        executable: executable.clone(),
        executable_sha256: actual_sha256,
        arguments_digest,
        argument_count,
        working_directory: working_directory.clone(),
        context_digest: config.execution_context.context_digest,
        sandbox_profile_digest,
        roots_snapshot_sha256: config.roots_snapshot_sha256,
        lifecycle: config.lifecycle,
        credential_scope: config.credential_scope,
        runtime_identity_digest,
    };
    Ok(PreparedLocalMcpServer {
        launch,
        executable,
        arguments: config.arguments,
        environment: config
            .execution_context
            .environment
            .variables
            .into_iter()
            .map(|(name, value)| (name, OsString::from(value)))
            .collect(),
        sandbox,
    })
}

#[derive(Serialize)]
struct LocalMcpRuntimeIdentityInput<'a> {
    schema_version: u16,
    server_id: &'a str,
    executable: &'a PathBuf,
    executable_sha256: &'a str,
    arguments_digest: &'a str,
    argument_count: u16,
    working_directory: &'a PathBuf,
    context_digest: &'a str,
    sandbox_profile_digest: &'a str,
    roots_snapshot_sha256: Option<&'a str>,
    lifecycle: LocalMcpServerLifecycle,
    credential_scope: LocalMcpCredentialScope,
}

fn validate_server_id(server_id: &str) -> Result<(), LocalMcpPlanError> {
    if server_id.is_empty()
        || server_id.len() > MAX_SERVER_ID_BYTES
        || !server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(LocalMcpPlanError::InvalidServerId);
    }
    Ok(())
}

fn validate_arguments(arguments: &[OsString]) -> Result<Vec<String>, LocalMcpPlanError> {
    if arguments.len() > MAX_ARGUMENTS {
        return Err(LocalMcpPlanError::TooManyArguments);
    }
    let mut total_bytes = 0_usize;
    arguments
        .iter()
        .map(|argument| {
            let argument = argument
                .to_str()
                .ok_or(LocalMcpPlanError::NonUtf8Argument)?;
            if argument.contains('\0') || argument.len() > MAX_ARGUMENT_BYTES {
                return Err(LocalMcpPlanError::InvalidArgument);
            }
            total_bytes = total_bytes
                .checked_add(argument.len())
                .ok_or(LocalMcpPlanError::ArgumentsTooLarge)?;
            if total_bytes > MAX_ARGUMENT_TOTAL_BYTES {
                return Err(LocalMcpPlanError::ArgumentsTooLarge);
            }
            Ok(argument.to_owned())
        })
        .collect()
}

fn canonical_exact_file(path: &PathBuf, label: &'static str) -> Result<PathBuf, LocalMcpPlanError> {
    if !path.is_absolute() {
        return Err(LocalMcpPlanError::Path {
            label,
            message: "path is not absolute".into(),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| LocalMcpPlanError::Path {
            label,
            message: error.to_string(),
        })?;
    if &canonical != path || !canonical.is_file() {
        return Err(LocalMcpPlanError::Path {
            label,
            message: "path must name the canonical regular file directly".into(),
        });
    }
    Ok(canonical)
}

fn canonical_exact_directory(path: &PathBuf) -> Result<PathBuf, LocalMcpPlanError> {
    if !path.is_absolute() {
        return Err(LocalMcpPlanError::Path {
            label: "MCP working directory",
            message: "path is not absolute".into(),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| LocalMcpPlanError::Path {
            label: "MCP working directory",
            message: error.to_string(),
        })?;
    if &canonical != path || !canonical.is_dir() {
        return Err(LocalMcpPlanError::Path {
            label: "MCP working directory",
            message: "path must name the canonical directory directly".into(),
        });
    }
    Ok(canonical)
}

fn validate_optional_sha256(value: Option<&str>) -> Result<(), LocalMcpPlanError> {
    if value.is_some_and(|value| {
        value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(LocalMcpPlanError::InvalidRootsSnapshotDigest);
    }
    Ok(())
}

fn sha256_json(value: &impl Serialize) -> Result<String, LocalMcpPlanError> {
    let bytes = serde_json::to_vec(value)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[derive(Debug, Error)]
pub enum LocalMcpPlanError {
    #[error("local MCP server id is invalid")]
    InvalidServerId,
    #[error("{label} is invalid: {message}")]
    Path {
        label: &'static str,
        message: String,
    },
    #[error("local MCP executable digest mismatch: expected {expected}, got {actual}")]
    ExecutableDigestMismatch { expected: String, actual: String },
    #[error("local MCP execution context is not hermetic")]
    UnsafeExecutionContext,
    #[error("local MCP working directory does not match its execution context")]
    WorkingDirectoryMismatch,
    #[error("local MCP sandbox is not a one-task, clear-environment profile")]
    UnsafeSandbox,
    #[error("local MCP has too many arguments")]
    TooManyArguments,
    #[error("local MCP argument is not UTF-8")]
    NonUtf8Argument,
    #[error("local MCP argument is invalid")]
    InvalidArgument,
    #[error("local MCP arguments exceed their byte bound")]
    ArgumentsTooLarge,
    #[error("local MCP roots snapshot digest is invalid")]
    InvalidRootsSnapshotDigest,
    #[error("local MCP sandbox is invalid: {0}")]
    Sandbox(String),
    #[error("local MCP digest is invalid: {0}")]
    Digest(String),
    #[error("local MCP JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local MCP executable inspection failed: {0}")]
    Driver(#[from] crate::DriverError),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use hyper_term_core::{ExecutionContextInputs, compile_execution_context};
    use hyper_term_protocol::{
        BindingLifetime, BindingScope, CollisionPolicy, EnvironmentBindingOrigin,
        EnvironmentBindingSpec, EnvironmentClass, EnvironmentPlan, EnvironmentSource,
        ExecutionContextSpec, OverridePolicy, RuntimeEnvironmentSpec, SandboxEnforcement,
        SandboxEnvironmentPolicy, SandboxFileSystemPolicy, SandboxNetworkPolicy,
        SandboxProcessPolicy, SandboxResourceLimits, WorkspaceContextSpec,
    };
    use tempfile::TempDir;

    use super::*;

    fn config(temp: &TempDir, arguments: &[&str]) -> LocalMcpServerConfig {
        let executable = temp.path().join("fixture-mcp");
        fs::copy("/bin/sh", &executable).unwrap();
        let executable = executable.canonicalize().unwrap();
        let workspace = temp.path().join("workspace");
        let home = temp.path().join("home");
        let scratch = temp.path().join("scratch");
        for directory in [&workspace, &home, &scratch] {
            fs::create_dir_all(directory).unwrap();
        }
        let workspace = workspace.canonicalize().unwrap();
        let profile = SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy::default(),
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy::default(),
            process: SandboxProcessPolicy::default(),
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneTask,
        };
        let spec = ExecutionContextSpec {
            schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
            context_id: "mcp:fixture:1".into(),
            context_revision: 1,
            mode: ExecutionMode::Hermetic,
            workspace: WorkspaceContextSpec {
                root: workspace.clone(),
                working_directory: workspace.clone(),
                runtime_home: home.canonicalize().unwrap(),
                runtime_temp: scratch.canonicalize().unwrap(),
            },
            runtime: RuntimeEnvironmentSpec {
                path: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
                locale: "C.UTF-8".into(),
                timezone: "UTC".into(),
                terminal: "dumb".into(),
            },
            shell: None,
            environment: EnvironmentPlan {
                bindings: vec![EnvironmentBindingSpec {
                    target_name: "MCP_FIXTURE_LABEL".into(),
                    source: EnvironmentSource::Literal {
                        value: "not-for-journal".into(),
                    },
                    class: EnvironmentClass::ToolConfiguration,
                    origin: EnvironmentBindingOrigin::Invocation,
                    scope: BindingScope::ProcessTree,
                    lifetime: BindingLifetime::Server,
                    override_policy: OverridePolicy::Deny,
                }],
                collision_policy: CollisionPolicy::Deny,
            },
            credentials: Vec::new(),
            sandbox: Some(profile),
        };
        let (execution_context, _) =
            compile_execution_context(&spec, &ExecutionContextInputs::default()).unwrap();
        LocalMcpServerConfig {
            server_id: "fixture_read".into(),
            executable_sha256: sha256_file(&executable).unwrap(),
            executable,
            arguments: arguments.iter().map(OsString::from).collect(),
            working_directory: workspace,
            execution_context,
            roots_snapshot_sha256: Some("a".repeat(64)),
            lifecycle: LocalMcpServerLifecycle::OneTask,
            credential_scope: LocalMcpCredentialScope::ServerLifetime,
        }
    }

    #[test]
    fn pinned_local_mcp_plan_is_deterministic_and_redacted() {
        let temp = TempDir::new().unwrap();
        let first = prepare_local_mcp_server(config(&temp, &["--stdio"])).unwrap();
        let second = prepare_local_mcp_server(config(&temp, &["--stdio"])).unwrap();
        assert_eq!(first.launch, second.launch);
        assert_eq!(first.launch.argument_count, 1);
        assert_eq!(
            first.environment.get("MCP_FIXTURE_LABEL"),
            Some(&OsString::from("not-for-journal"))
        );
        let recorded = serde_json::to_string(&first.launch).unwrap();
        assert!(!recorded.contains("not-for-journal"));
        assert!(!recorded.contains("--stdio"));
    }

    #[test]
    fn arguments_are_bound_into_the_runtime_identity() {
        let temp = TempDir::new().unwrap();
        let first = prepare_local_mcp_server(config(&temp, &["--read-only"])).unwrap();
        let second = prepare_local_mcp_server(config(&temp, &["--write"])).unwrap();
        assert_ne!(
            first.launch.arguments_digest,
            second.launch.arguments_digest
        );
        assert_ne!(
            first.launch.runtime_identity_digest,
            second.launch.runtime_identity_digest
        );
    }

    #[test]
    fn mutable_or_substituted_executable_fails_before_launch() {
        let temp = TempDir::new().unwrap();
        let mut digest_mismatch = config(&temp, &[]);
        digest_mismatch.executable_sha256 = "0".repeat(64);
        assert!(matches!(
            prepare_local_mcp_server(digest_mismatch),
            Err(LocalMcpPlanError::ExecutableDigestMismatch { .. })
        ));

        let mut symlink = config(&temp, &[]);
        let alias = temp.path().join("fixture-alias");
        std::os::unix::fs::symlink(&symlink.executable, &alias).unwrap();
        symlink.executable = alias;
        assert!(matches!(
            prepare_local_mcp_server(symlink),
            Err(LocalMcpPlanError::Path { .. })
        ));
    }

    #[test]
    fn user_mode_or_unbounded_lifetime_fails_closed() {
        let temp = TempDir::new().unwrap();
        let mut user = config(&temp, &[]);
        user.execution_context.mode = ExecutionMode::User;
        assert!(matches!(
            prepare_local_mcp_server(user),
            Err(LocalMcpPlanError::UnsafeExecutionContext)
        ));

        let mut lifetime = config(&temp, &[]);
        lifetime
            .execution_context
            .requested_sandbox
            .as_mut()
            .unwrap()
            .lifetime = SandboxLifetime::OneOperation;
        assert!(matches!(
            prepare_local_mcp_server(lifetime),
            Err(LocalMcpPlanError::UnsafeSandbox)
        ));
    }

    #[test]
    fn runtime_identity_contains_no_raw_environment() {
        let temp = TempDir::new().unwrap();
        let prepared = prepare_local_mcp_server(config(&temp, &[])).unwrap();
        let launch = serde_json::to_value(prepared.launch).unwrap();
        assert_eq!(launch["credential_scope"], "server_lifetime");
        assert_eq!(launch["lifecycle"], "one_task");
        assert!(launch.get("environment").is_none());
        assert!(launch.get("credentials").is_none());
        assert_eq!(
            prepared.environment,
            BTreeMap::from([
                (
                    "HOME".into(),
                    OsString::from(temp.path().join("home").canonicalize().unwrap())
                ),
                ("LANG".into(), OsString::from("C.UTF-8")),
                (
                    "MCP_FIXTURE_LABEL".into(),
                    OsString::from("not-for-journal")
                ),
                ("PATH".into(), OsString::from("/usr/bin:/bin")),
                ("TERM".into(), OsString::from("dumb")),
                (
                    "TMPDIR".into(),
                    OsString::from(temp.path().join("scratch").canonicalize().unwrap())
                ),
                ("TZ".into(), OsString::from("UTC")),
            ])
        );
    }
}
