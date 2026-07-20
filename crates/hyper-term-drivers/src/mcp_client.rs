use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use hyper_term_core::{
    OperationRecord, SandboxCompileRequest, SandboxLauncher, sandbox_profile_digest,
};
use hyper_term_protocol::{
    Actor, EXECUTION_CONTEXT_SCHEMA_VERSION, ExecutionMode, LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
    LocalMcpCredentialScope, LocalMcpServerLaunch, LocalMcpServerLifecycle,
    LocalMcpServerRuntimeReceipt, LocalMcpToolContractReceipt, McpArgumentsDigest,
    McpCapabilitiesDigest, McpCatalogDigest, McpRuntimeIdentityDigest, McpToolContractDigest,
    OperationAction, OperationKind, OperationState, ResolvedExecutionContext, SandboxLifetime,
    SandboxProfile, TerminalCommand,
};
use hyper_term_sandbox::MacOsSeatbeltLauncher;
use process_wrap::tokio::{CommandWrap, KillOnDrop, ProcessGroup};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::sha256_file;

const MAX_SERVER_ID_BYTES: usize = 64;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024;
const MAX_DISCOVERED_TOOLS: usize = 256;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_SCHEMA_BYTES: usize = 1024 * 1024;
const STDERR_TAIL_BYTES: usize = 64 * 1024;

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

pub struct AuthorizedLocalMcpServer {
    prepared: PreparedLocalMcpServer,
    sandbox_launch: hyper_term_core::SandboxLaunchPlan,
}

pub struct ManagedLocalMcpClient {
    service: RunningService<RoleClient, ()>,
    receipt: LocalMcpServerRuntimeReceipt,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    stderr_reader: JoinHandle<()>,
}

pub fn authorize_local_mcp_server(
    prepared: PreparedLocalMcpServer,
    operation: &OperationRecord,
) -> Result<AuthorizedLocalMcpServer, LocalMcpPlanError> {
    if operation.kind != OperationKind::McpServerLaunch
        || operation.state != OperationState::Dispatching
        || operation.revision == 0
        || !matches!(
            &operation.action,
            OperationAction::McpServerLaunch { launch } if launch == &prepared.launch
        )
    {
        return Err(LocalMcpPlanError::LaunchNotAuthorized);
    }
    let command = TerminalCommand {
        program: prepared
            .executable
            .to_str()
            .ok_or(LocalMcpPlanError::NonUtf8Executable)?
            .to_owned(),
        args: validate_arguments(&prepared.arguments)?,
        cwd: Some(prepared.launch.working_directory.clone()),
        env: prepared
            .environment
            .iter()
            .map(|(name, value)| {
                value
                    .to_str()
                    .map(|value| (name.clone(), value.to_owned()))
                    .ok_or(LocalMcpPlanError::NonUtf8Environment)
            })
            .collect::<Result<_, _>>()?,
    };
    let sandbox_launch = MacOsSeatbeltLauncher
        .compile(&SandboxCompileRequest {
            operation_id: operation.operation_id,
            operation_revision: operation.revision,
            actor: Actor::System,
            command,
            profile: prepared.sandbox.clone(),
        })
        .map_err(|error| LocalMcpPlanError::Sandbox(error.to_string()))?;
    if !sandbox_launch.compiled.enforced || !sandbox_launch.clear_environment {
        return Err(LocalMcpPlanError::UnsafeSandbox);
    }
    Ok(AuthorizedLocalMcpServer {
        prepared,
        sandbox_launch,
    })
}

impl ManagedLocalMcpClient {
    pub async fn launch(
        authorized: AuthorizedLocalMcpServer,
        startup_timeout: Duration,
        discovery_timeout: Duration,
    ) -> Result<Self, LocalMcpClientError> {
        let mut command = tokio::process::Command::new(&authorized.sandbox_launch.command.program);
        command
            .args(&authorized.sandbox_launch.command.args)
            .current_dir(
                authorized
                    .sandbox_launch
                    .command
                    .cwd
                    .as_ref()
                    .ok_or(LocalMcpClientError::InvalidSandboxLaunch)?,
            )
            .env_clear()
            .envs(&authorized.sandbox_launch.command.env);
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        command.wrap(KillOnDrop);
        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(LocalMcpClientError::Spawn)?;
        let stderr = stderr.ok_or(LocalMcpClientError::MissingStderr)?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)));
        let stderr_reader = spawn_stderr_reader(stderr, Arc::clone(&stderr_tail));
        let service = match tokio::time::timeout(startup_timeout, ().serve(transport)).await {
            Ok(Ok(service)) => service,
            Ok(Err(error)) => {
                let tail = stderr_snapshot(&stderr_tail).await;
                stderr_reader.abort();
                return Err(LocalMcpClientError::Initialize(format!(
                    "{error}; stderr: {tail}"
                )));
            }
            Err(_) => {
                let tail = stderr_snapshot(&stderr_tail).await;
                stderr_reader.abort();
                return Err(LocalMcpClientError::StartupTimeout(tail));
            }
        };
        let tools = match tokio::time::timeout(discovery_timeout, service.list_all_tools()).await {
            Ok(Ok(tools)) => tools,
            Ok(Err(error)) => {
                drop(service);
                stderr_reader.abort();
                return Err(LocalMcpClientError::Discovery(error.to_string()));
            }
            Err(_) => {
                drop(service);
                stderr_reader.abort();
                return Err(LocalMcpClientError::DiscoveryTimeout);
            }
        };
        let receipt = build_runtime_receipt(
            authorized.prepared.launch,
            authorized.sandbox_launch.compiled.profile_digest.clone(),
            service
                .peer_info()
                .ok_or(LocalMcpClientError::MissingServerInfo)?
                .as_ref(),
            tools,
        )?;
        Ok(Self {
            service,
            receipt,
            stderr_tail,
            stderr_reader,
        })
    }

    pub fn receipt(&self) -> &LocalMcpServerRuntimeReceipt {
        &self.receipt
    }

    pub async fn stderr_tail(&self) -> String {
        let mut tail = self.stderr_tail.lock().await;
        String::from_utf8_lossy(tail.make_contiguous()).into_owned()
    }

    pub async fn close(&mut self, timeout: Duration) -> Result<(), LocalMcpClientError> {
        let closed = self
            .service
            .close_with_timeout(timeout)
            .await
            .map_err(|error| LocalMcpClientError::Close(error.to_string()))?;
        self.stderr_reader.abort();
        if closed.is_none() {
            return Err(LocalMcpClientError::CloseTimeout);
        }
        Ok(())
    }
}

impl Drop for ManagedLocalMcpClient {
    fn drop(&mut self) {
        self.stderr_reader.abort();
    }
}

fn build_runtime_receipt(
    launch: LocalMcpServerLaunch,
    enforced_sandbox_profile_digest: hyper_term_protocol::SandboxProfileDigest,
    server: &rmcp::model::ServerInfo,
    mut tools: Vec<rmcp::model::Tool>,
) -> Result<LocalMcpServerRuntimeReceipt, LocalMcpClientError> {
    if server.server_info.name.is_empty()
        || server.server_info.name.len() > 256
        || server.server_info.version.is_empty()
        || server.server_info.version.len() > 128
        || server
            .server_info
            .name
            .chars()
            .chain(server.server_info.version.chars())
            .any(char::is_control)
    {
        return Err(LocalMcpClientError::InvalidServerInfo);
    }
    if tools.len() > MAX_DISCOVERED_TOOLS {
        return Err(LocalMcpClientError::TooManyTools);
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let mut previous_name: Option<String> = None;
    let mut tool_receipts = Vec::with_capacity(tools.len());
    for tool in tools {
        let name = tool.name.into_owned();
        if name.is_empty()
            || name.len() > MAX_TOOL_NAME_BYTES
            || name.chars().any(char::is_control)
            || previous_name.as_deref() == Some(name.as_str())
        {
            return Err(LocalMcpClientError::InvalidToolCatalog);
        }
        let input_schema_sha256 = bounded_json_sha256(
            tool.input_schema.as_ref(),
            MAX_TOOL_SCHEMA_BYTES,
            "input schema",
        )?;
        let output_schema_sha256 = tool
            .output_schema
            .as_ref()
            .map(|schema| {
                bounded_json_sha256(schema.as_ref(), MAX_TOOL_SCHEMA_BYTES, "output schema")
            })
            .transpose()?;
        let contract_digest =
            McpToolContractDigest::parse(client_sha256_json(&McpToolContractInput {
                planned_runtime_identity: launch.runtime_identity_digest.as_str(),
                name: &name,
                input_schema_sha256: &input_schema_sha256,
                output_schema_sha256: output_schema_sha256.as_deref(),
            })?)
            .map_err(|error| LocalMcpClientError::Digest(error.to_string()))?;
        previous_name = Some(name.clone());
        tool_receipts.push(LocalMcpToolContractReceipt {
            name,
            input_schema_sha256,
            output_schema_sha256,
            contract_digest,
        });
    }
    let capabilities_digest = McpCapabilitiesDigest::parse(bounded_json_sha256(
        &server.capabilities,
        MAX_TOOL_SCHEMA_BYTES,
        "server capabilities",
    )?)
    .map_err(|error| LocalMcpClientError::Digest(error.to_string()))?;
    let catalog_digest = McpCatalogDigest::parse(client_sha256_json(&tool_receipts)?)
        .map_err(|error| LocalMcpClientError::Digest(error.to_string()))?;
    let negotiated_protocol_version = server.protocol_version.to_string();
    let runtime_identity_digest =
        McpRuntimeIdentityDigest::parse(client_sha256_json(&NegotiatedMcpRuntimeIdentityInput {
            planned_runtime_identity: launch.runtime_identity_digest.as_str(),
            negotiated_protocol_version: &negotiated_protocol_version,
            server_name: &server.server_info.name,
            server_version: &server.server_info.version,
            enforced_sandbox_profile_digest: enforced_sandbox_profile_digest.as_str(),
            capabilities_digest: capabilities_digest.as_str(),
            catalog_digest: catalog_digest.as_str(),
        })?)
        .map_err(|error| LocalMcpClientError::Digest(error.to_string()))?;
    Ok(LocalMcpServerRuntimeReceipt {
        schema_version: LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
        credential_scope: launch.credential_scope,
        launch,
        negotiated_protocol_version,
        server_name: server.server_info.name.clone(),
        server_version: server.server_info.version.clone(),
        enforced_sandbox_profile_digest,
        capabilities_digest,
        catalog_digest,
        runtime_identity_digest,
        tools: tool_receipts,
        per_call_isolation: false,
    })
}

fn spawn_stderr_reader(
    mut stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<u8>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 4096];
        loop {
            let read = match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let mut tail = tail.lock().await;
            let overflow = tail
                .len()
                .saturating_add(read)
                .saturating_sub(STDERR_TAIL_BYTES);
            if overflow > 0 {
                let retained = tail.len();
                tail.drain(..overflow.min(retained));
            }
            tail.extend(&buffer[..read]);
        }
    })
}

async fn stderr_snapshot(tail: &Arc<Mutex<VecDeque<u8>>>) -> String {
    let mut tail = tail.lock().await;
    String::from_utf8_lossy(tail.make_contiguous()).into_owned()
}

#[derive(Serialize)]
struct McpToolContractInput<'a> {
    planned_runtime_identity: &'a str,
    name: &'a str,
    input_schema_sha256: &'a str,
    output_schema_sha256: Option<&'a str>,
}

#[derive(Serialize)]
struct NegotiatedMcpRuntimeIdentityInput<'a> {
    planned_runtime_identity: &'a str,
    negotiated_protocol_version: &'a str,
    server_name: &'a str,
    server_version: &'a str,
    enforced_sandbox_profile_digest: &'a str,
    capabilities_digest: &'a str,
    catalog_digest: &'a str,
}

fn bounded_json_sha256(
    value: &impl Serialize,
    maximum: usize,
    label: &'static str,
) -> Result<String, LocalMcpClientError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > maximum {
        return Err(LocalMcpClientError::CatalogValueTooLarge { label, maximum });
    }
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn client_sha256_json(value: &impl Serialize) -> Result<String, LocalMcpClientError> {
    let bytes = serde_json::to_vec(value)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
    #[error("local MCP executable path is not UTF-8")]
    NonUtf8Executable,
    #[error("local MCP environment is not UTF-8")]
    NonUtf8Environment,
    #[error("local MCP argument is invalid")]
    InvalidArgument,
    #[error("local MCP arguments exceed their byte bound")]
    ArgumentsTooLarge,
    #[error("local MCP roots snapshot digest is invalid")]
    InvalidRootsSnapshotDigest,
    #[error("local MCP server launch has not entered its exact dispatching operation")]
    LaunchNotAuthorized,
    #[error("local MCP sandbox is invalid: {0}")]
    Sandbox(String),
    #[error("local MCP digest is invalid: {0}")]
    Digest(String),
    #[error("local MCP JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local MCP executable inspection failed: {0}")]
    Driver(#[from] crate::DriverError),
}

#[derive(Debug, Error)]
pub enum LocalMcpClientError {
    #[error("cannot spawn the authorized local MCP server: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("authorized local MCP server did not expose stderr")]
    MissingStderr,
    #[error("authorized local MCP sandbox launch is incomplete")]
    InvalidSandboxLaunch,
    #[error("local MCP initialization timed out; stderr: {0}")]
    StartupTimeout(String),
    #[error("local MCP initialization failed: {0}")]
    Initialize(String),
    #[error("local MCP tool discovery timed out")]
    DiscoveryTimeout,
    #[error("local MCP tool discovery failed: {0}")]
    Discovery(String),
    #[error("local MCP server returned no negotiated identity")]
    MissingServerInfo,
    #[error("local MCP server identity is invalid")]
    InvalidServerInfo,
    #[error("local MCP server advertised too many tools")]
    TooManyTools,
    #[error("local MCP server advertised an invalid or duplicate tool")]
    InvalidToolCatalog,
    #[error("local MCP {label} exceeds the {maximum}-byte bound")]
    CatalogValueTooLarge { label: &'static str, maximum: usize },
    #[error("local MCP digest is invalid: {0}")]
    Digest(String),
    #[error("local MCP runtime receipt JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local MCP shutdown failed: {0}")]
    Close(String),
    #[error("local MCP process tree did not close within its deadline")]
    CloseTimeout,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use hyper_term_core::{ExecutionContextInputs, compile_execution_context};
    use hyper_term_protocol::{
        BindingLifetime, BindingScope, CollisionPolicy, EnvironmentBindingOrigin,
        EnvironmentBindingSpec, EnvironmentClass, EnvironmentPlan, EnvironmentSource,
        ExecutionContextSpec, OperationId, OverridePolicy, RiskClass, RuntimeEnvironmentSpec,
        SandboxEnforcement, SandboxEnvironmentPolicy, SandboxFileSystemPolicy,
        SandboxNetworkPolicy, SandboxProcessPolicy, SandboxResourceLimits, TaskId,
        WorkspaceContextSpec,
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

    fn operation(launch: LocalMcpServerLaunch, state: OperationState) -> OperationRecord {
        OperationRecord {
            task_id: TaskId::new(),
            operation_id: OperationId::new(),
            revision: 4,
            kind: OperationKind::McpServerLaunch,
            action: OperationAction::McpServerLaunch { launch },
            summary: "Start reviewed fixture MCP".into(),
            risk: RiskClass::ExternalEffect,
            required_capabilities: vec!["mcp.server.launch".into()],
            state,
            permission_decision: Some(hyper_term_protocol::PermissionDecision::AllowOnce),
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
    fn only_the_exact_dispatching_operation_can_authorize_spawn() {
        let temp = TempDir::new().unwrap();
        let prepared = prepare_local_mcp_server(config(&temp, &[])).unwrap();
        let waiting = operation(prepared.launch.clone(), OperationState::WaitingHuman);
        assert!(matches!(
            authorize_local_mcp_server(prepared.clone(), &waiting),
            Err(LocalMcpPlanError::LaunchNotAuthorized)
        ));

        let mut substituted = operation(prepared.launch.clone(), OperationState::Dispatching);
        if let OperationAction::McpServerLaunch { launch } = &mut substituted.action {
            launch.argument_count += 1;
        }
        assert!(matches!(
            authorize_local_mcp_server(prepared.clone(), &substituted),
            Err(LocalMcpPlanError::LaunchNotAuthorized)
        ));

        #[cfg(target_os = "macos")]
        {
            let dispatching = operation(prepared.launch.clone(), OperationState::Dispatching);
            authorize_local_mcp_server(prepared, &dispatching).unwrap();
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn authorized_stdio_server_negotiates_a_bounded_catalog_and_closes() {
        let temp = TempDir::new().unwrap();
        let script = r#"
printf 'fixture MCP ready\n' >&2
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"fixture-mcp","version":"1.0.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"fixture.read","description":"Read fixture metadata","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]},"outputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}'
      ;;
  esac
done
"#;
        let mut fixture_config = config(&temp, &["-c", script]);
        fixture_config.executable = PathBuf::from("/bin/sh").canonicalize().unwrap();
        fixture_config.executable_sha256 = sha256_file(&fixture_config.executable).unwrap();
        let prepared = prepare_local_mcp_server(fixture_config).unwrap();
        let dispatching = operation(prepared.launch.clone(), OperationState::Dispatching);
        let authorized = authorize_local_mcp_server(prepared, &dispatching).unwrap();
        let mut fixture = std::process::Command::new(&authorized.sandbox_launch.command.program)
            .args(&authorized.sandbox_launch.command.args)
            .current_dir(authorized.sandbox_launch.command.cwd.as_ref().unwrap())
            .env_clear()
            .envs(&authorized.sandbox_launch.command.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        fixture
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{}}\n")
            .unwrap();
        let fixture_output = fixture.wait_with_output().unwrap();
        assert!(
            fixture_output.status.success(),
            "status={:?} stderr={}",
            fixture_output.status.code(),
            String::from_utf8_lossy(&fixture_output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&fixture_output.stdout).contains("fixture-mcp"),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&fixture_output.stdout),
            String::from_utf8_lossy(&fixture_output.stderr)
        );
        let mut client = ManagedLocalMcpClient::launch(
            authorized,
            Duration::from_secs(2),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        assert_eq!(client.receipt().negotiated_protocol_version, "2025-11-25");
        assert_eq!(client.receipt().server_name, "fixture-mcp");
        assert_eq!(client.receipt().server_version, "1.0.0");
        assert_eq!(client.receipt().tools.len(), 1);
        assert_eq!(client.receipt().tools[0].name, "fixture.read");
        assert!(!client.receipt().per_call_isolation);
        assert_eq!(
            client.receipt().credential_scope,
            LocalMcpCredentialScope::ServerLifetime
        );
        assert!(client.stderr_tail().await.contains("fixture MCP ready"));
        let receipt = serde_json::to_string(client.receipt()).unwrap();
        assert!(!receipt.contains("Read fixture metadata"));
        assert!(!receipt.contains("while IFS="));
        client.close(Duration::from_secs(2)).await.unwrap();
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
