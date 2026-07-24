use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as RoutePath, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use hyper_term_core::OperationRecord;
use hyper_term_drivers::{
    AcpAgentClient, AcpAgentConfig, AcpMcpServerConfig, AgentContainmentConfig, AgentDriverEvent,
    AgentEffectAuthorization, AgentEffectKind, AgentGoalStatus, AgentHostOperation,
    AgentHostRequest, AgentHostResponse, AgentSessionCapabilities, AgentThreadGoal,
    CodexAppServerClient, CodexAppServerConfig, CodexMcpServerConfig, DenoGenUiCompiler,
    DenoGenUiConfig, DriverState, ExternalRequestId, GenUiCompileRequest, LocalMcpServerConfig,
    StructuredAgentClient, StructuredAgentProtocol, sha256_file, stage_codex_auth_file,
};
use hyper_term_protocol::{
    AcceptedGenUiArtifact, ArtifactId, BlockId, BlockKind, BlockPatch, GenUiArtifactCandidate,
    GenUiBugCapsule, GenUiBugCapsuleEnvironment, GenUiRuntimeTraceAppendRequest,
    GenUiRuntimeTraceProjection, LocalMcpServerRuntimeReceipt, LocalMcpToolCallReceipt,
    MessageRole, OperationAction, OperationCompletion, OperationId, OperationKind,
    OperationOutcome, OperationState, PermissionDecision, RiskClass, TaskId, TerminalCommand,
};
use hyper_term_sandbox::{IsolatedTaskTermination, LimaTaskRunner};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::acp_provider_home::{
    stage_acp_claude_home, stage_acp_codex_preferences, stage_acp_copilot_home,
};
use crate::agent_provider_probe::{self, AgentProviderProbeConfig};
pub use crate::agent_provider_probe::{
    AcpAgentProviderConfig, AgentProviderContainment, AgentProviderReadiness, AgentProviderStatus,
};
use crate::agent_session_store::AgentSessionBindingStore;
use crate::artifact_debug_capsule::build_bug_capsule;
use crate::artifact_editor_store::{
    ArtifactEditorCheckpoint, ArtifactEditorCheckpointRequest, ArtifactEditorStore,
    ArtifactEditorStoreError,
};
use crate::artifact_runtime_trace_store::{ArtifactRuntimeTraceStore, RuntimeTraceStoreError};
use crate::artifact_visual_quality_store::ArtifactVisualQualityStore;
use crate::editor_lsp::{EditorLspError, EditorLspRequest, EditorLspResponse, EditorLspService};
use crate::network_proxy::ManagedConnectProxy;
use crate::private_fs::ensure_private_directory;
use crate::workspace_apply::{
    DurableWorkspaceApplyResult, MAX_WORKSPACE_APPLY_FILES, WorkspaceApplyError,
    WorkspaceApplySetPlan, WorkspaceRecoveryReport, WorkspaceTransactionContext,
    WorkspaceTransactionOutcome, WorkspaceTransactionReceipt, acknowledge_workspace_transaction,
    apply_workspace_set_plan_durable, prepare_workspace_apply_set, recover_workspace_transactions,
    select_workspace_apply_set,
};
use crate::workspace_diff::{
    MAX_WORKSPACE_HUNKS_PER_FILE, WorkspaceDiffReview, review_workspace_diff,
    select_workspace_hunks,
};
use crate::workspace_snapshot::{create_private_runtime_root, create_workspace_snapshot};
use crate::{
    AgentCapabilityPolicy, BrokeredMcpRuntimeConfig, DaemonError, DaemonState,
    DenoGenUiMcpExecutorConfig, DenoMcpExecutorConfig, IsolatedAcceptancePreview,
    IsolatedAcceptanceReview, LocalMcpRuntimeError, LocalMcpRuntimeManager, UnixServerHandle,
    spawn_agent_capability_server_with_policy,
};

mod view_model;
use view_model::*;
mod http_handlers;
use http_handlers::*;
mod agent_turn;
use agent_turn::{
    AgentTurnProjection, bounded_error, continue_turn, execute_agent_terminal_create,
    permission_decision_failure, projected_agent_status, run_turn, set_progress_failed,
};
#[cfg(test)]
use agent_turn::{agent_error_summary, retain_terminal_output};
mod launch_provider;
mod mcp_launch;
mod permission;
mod runtime_artifact;
mod runtime_operations;
mod runtime_session;
use permission::decide_permission;
mod artifact_quality;
use artifact_quality::{
    artifact_render_payload, artifact_visual_quality, submit_artifact_visual_quality,
};
mod startup;
#[cfg(test)]
mod test_support;

const MIN_TOKEN_BYTES: usize = 32;
const MAX_AGENT_SESSIONS: usize = 8;
const MAX_AGENT_TERMINALS: usize = 64;
const START_TURN_TIMEOUT: Duration = Duration::from_secs(10);
const COMPLETE_TURN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_PROMPT_BYTES: usize = 16 * 1024;
const MAX_AGENT_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_AGENT_STREAM_LINE_BYTES: usize = 256 * 1024;
const AGENT_STREAM_PATCH_QUEUE: usize = 512;
const AGENT_STREAM_REFRESH: Duration = Duration::from_millis(75);
const AGENT_STREAM_FRAME_CADENCE: Duration = Duration::from_millis(16);
const AGENT_CSP: &str = "default-src 'none'; frame-ancestors 'none'";
const PREVIEW_CSP: &str = "default-src 'none'; script-src 'unsafe-inline' blob:; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; font-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const WORKBENCH_CSP: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; frame-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const WORKBENCH_PREVIEW_CSP: &str = "default-src 'none'; script-src 'unsafe-inline' blob:; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; font-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";
const MAX_PREVIEW_SHELL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_WORKBENCH_ASSET_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EDITOR_LSP_BODY_BYTES: usize = 1024 * 1024 + 64 * 1024;
const MAX_TIER2_PREVIEW_CHANGES: usize = 32;
const MAX_TIER2_PREVIEW_PATCH_BYTES: usize = 64 * 1024;
const MAX_ACP_SHEBANG_BYTES: usize = 512;
const ARTIFACT_BOOTSTRAP_MARKER: &str = "<!-- HYPER_TERM_ARTIFACT_BOOTSTRAP -->";
const CODEX_NETWORK_ALLOWED_HOSTS: &[&str] = &["api.openai.com", "auth.openai.com", "chatgpt.com"];
const CLAUDE_NETWORK_ALLOWED_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "*.anthropic.com",
    "claude.ai",
    "*.claude.ai",
];
const COPILOT_NETWORK_ALLOWED_HOSTS: &[&str] = &[
    "api.github.com",
    "github.com",
    "*.githubcopilot.com",
    "copilot-proxy.githubusercontent.com",
];

#[derive(Clone)]
pub struct AgentGatewayConfig {
    pub bind: SocketAddr,
    pub token: String,
    pub workspace: PathBuf,
    pub state_directory: PathBuf,
    pub daemon: DaemonState,
    pub provider_home: PathBuf,
    pub codex_executable: Option<PathBuf>,
    pub codex_auth_file: Option<PathBuf>,
    pub acp_providers: Vec<AcpAgentProviderConfig>,
    pub local_mcp_servers: Vec<LocalMcpServerConfig>,
    pub mcp_executable: Option<PathBuf>,
    pub genui_runtime: Option<AgentGenUiRuntimeConfig>,
    pub workbench_assets: Option<PathBuf>,
    pub debug_capsule: Option<GenUiBugCapsule>,
    pub tier2_runner: Option<LimaTaskRunner>,
    pub control_socket: PathBuf,
}

#[derive(Clone, Debug)]
pub struct AgentGenUiRuntimeConfig {
    pub deno_executable: PathBuf,
    pub runtime_version: String,
    pub compiler_script: PathBuf,
    pub compiler_wasm: PathBuf,
    pub preview_shell: PathBuf,
    pub compiler_version: String,
}

pub struct AgentGatewayHandle {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), std::io::Error>>>,
    runtime: AgentGatewayRuntime,
}

impl AgentGatewayHandle {
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub async fn shutdown(mut self) -> Result<(), AgentGatewayError> {
        self.runtime.local_mcp.close_all().await;
        self.runtime.close_all();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}

impl Drop for AgentGatewayHandle {
    fn drop(&mut self) {
        self.runtime.close_all();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentGatewayError {
    #[error("agent gateway must bind to a loopback address, got {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("agent gateway token must contain at least {MIN_TOKEN_BYTES} bytes")]
    WeakToken,
    #[error("agent gateway workspace is invalid: {0}")]
    InvalidWorkspace(PathBuf),
    #[error("agent gateway state directory is invalid: {0}")]
    InvalidStateDirectory(PathBuf),
    #[error("agent gateway provider home is invalid: {0}")]
    InvalidProviderHome(PathBuf),
    #[error("agent gateway GenUI runtime is invalid: {0}")]
    InvalidGenUiRuntime(String),
    #[error("agent gateway Workbench assets are invalid: {0}")]
    InvalidWorkbenchAssets(PathBuf),
    #[error("agent gateway workspace recovery failed: {0}")]
    WorkspaceRecovery(String),
    #[error("agent gateway ACP provider is invalid: {0}")]
    InvalidAcpProvider(String),
    #[error("agent gateway local MCP runtime is invalid: {0}")]
    InvalidLocalMcpRuntime(String),
    #[error("agent gateway I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent gateway task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
struct AgentGatewayRuntime {
    config: Arc<AgentGatewayConfig>,
    local_mcp: Arc<LocalMcpRuntimeManager>,
    sessions: Arc<Mutex<HashMap<u16, Arc<AgentSession>>>>,
    session_bindings: Arc<AgentSessionBindingStore>,
    preview_shell: Option<Arc<str>>,
    preview_runtime_digest: Option<Arc<str>>,
    workbench_assets: Option<Arc<PathBuf>>,
    editor_lsp: Option<Arc<EditorLspService>>,
    artifact_draft_compiler: Option<Arc<ArtifactDraftCompiler>>,
    artifact_editor_store: Arc<ArtifactEditorStore>,
    artifact_editor_lock: Arc<Mutex<()>>,
    artifact_runtime_trace_store: Arc<ArtifactRuntimeTraceStore>,
    artifact_runtime_trace_lock: Arc<Mutex<()>>,
    artifact_visual_quality_store: Arc<ArtifactVisualQualityStore>,
    artifact_visual_quality_lock: Arc<Mutex<()>>,
    artifact_drafts: Arc<Mutex<HashMap<OperationId, ArtifactDraftRecord>>>,
    workspace_applies: Arc<Mutex<HashMap<OperationId, WorkspaceApplyRecord>>>,
    workspace_recovery_block: Arc<Mutex<Option<String>>>,
}

struct ArtifactDraftCompiler {
    config: DenoGenUiConfig,
    compiler: Mutex<Option<Arc<DenoGenUiCompiler>>>,
}

#[derive(Clone)]
struct ArtifactDraftRecord {
    session_id: u16,
    task_id: TaskId,
    base_artifact_id: ArtifactId,
    base_source_revision: u64,
    waiting_revision: u64,
    request: GenUiCompileRequest,
    state: ArtifactDraftState,
}

#[derive(Clone)]
enum ArtifactDraftState {
    WaitingApproval,
    Compiling,
    Accepted(AcceptedGenUiArtifact),
    Rejected,
    Failed(String),
}

#[derive(Clone)]
struct WorkspaceApplyRecord {
    session_id: u16,
    task_id: TaskId,
    artifact_id: ArtifactId,
    artifact_source_revision: u64,
    source_paths: Vec<String>,
    artifact_source_digests: Vec<String>,
    selected_hunk_count: usize,
    waiting_revision: u64,
    plan: WorkspaceApplySetPlan,
    state: WorkspaceApplyState,
}

struct PreparedWorkspaceReview {
    artifact_source_revision: u64,
    source_paths: Vec<String>,
    artifact_source_digests: Vec<String>,
    plan: WorkspaceApplySetPlan,
    diffs: Vec<WorkspaceDiffReview>,
    review_digest: String,
}

#[derive(Clone)]
enum WorkspaceApplyState {
    WaitingApproval,
    Applying,
    Applied,
    Rejected,
    Failed(String),
    UnknownExecution(String),
}

struct AgentSession {
    client: Arc<dyn StructuredAgentClient>,
    provider_id: String,
    protocol: StructuredAgentProtocol,
    task_id: TaskId,
    thread_id: String,
    history_restored: bool,
    runtime_root: PathBuf,
    progress: Mutex<AgentProgress>,
    pending_effect: Mutex<Option<PendingAgentEffect>>,
    terminals: Mutex<HashMap<String, AgentTerminalRecord>>,
    _managed_proxy: Option<ManagedConnectProxy>,
    capability_server: Option<UnixServerHandle>,
}

struct LaunchedAgentProvider {
    client: Arc<dyn StructuredAgentClient>,
    managed_proxy: Option<ManagedConnectProxy>,
    capability_server: Option<UnixServerHandle>,
}

#[derive(Clone)]
struct PendingAgentEffect {
    request_id: ExternalRequestId,
    payload_sha256: String,
    operation_id: OperationId,
    operation_revision: u64,
    projection: AgentTurnProjection,
    host_request: Option<AgentHostRequest>,
}

#[derive(Clone)]
struct AgentTerminalRecord {
    _source_operation_id: OperationId,
    output: String,
    truncated: bool,
    exit_code: Option<u32>,
    signal: Option<String>,
}

struct BrokeredMcpLaunch {
    executable: PathBuf,
    executable_sha256: String,
    arguments: Vec<OsString>,
    runtime_home: PathBuf,
    runtime_temp: PathBuf,
    capability_socket: PathBuf,
    capability_server: Option<UnixServerHandle>,
}

struct AgentProgress {
    status: AgentStatus,
    turn_id: Option<String>,
    failure: Option<AgentFailure>,
}

fn provider_probe_config(config: &AgentGatewayConfig) -> AgentProviderProbeConfig<'_> {
    AgentProviderProbeConfig {
        provider_home: &config.provider_home,
        codex_executable: config.codex_executable.as_deref(),
        codex_auth_file: config.codex_auth_file.as_deref(),
        acp_providers: &config.acp_providers,
    }
}

pub fn probe_agent_provider_statuses(config: &AgentGatewayConfig) -> Vec<AgentProviderStatus> {
    agent_provider_probe::probe_agent_provider_statuses(provider_probe_config(config))
}

fn probe_known_agent_provider(
    config: &AgentGatewayConfig,
    provider_id: &str,
) -> Option<AgentProviderStatus> {
    probe_agent_provider_statuses(config)
        .into_iter()
        .find(|status| status.id == provider_id)
}

pub async fn spawn_agent_gateway(
    mut config: AgentGatewayConfig,
) -> Result<AgentGatewayHandle, AgentGatewayError> {
    if !config.bind.ip().is_loopback() {
        return Err(AgentGatewayError::NonLoopbackBind(config.bind));
    }
    if config.token.len() < MIN_TOKEN_BYTES {
        return Err(AgentGatewayError::WeakToken);
    }
    config.workspace = config
        .workspace
        .canonicalize()
        .map_err(|_| AgentGatewayError::InvalidWorkspace(config.workspace.clone()))?;
    config.provider_home = config
        .provider_home
        .canonicalize()
        .map_err(|_| AgentGatewayError::InvalidProviderHome(config.provider_home.clone()))?;
    if !config.provider_home.is_dir() {
        return Err(AgentGatewayError::InvalidProviderHome(
            config.provider_home.clone(),
        ));
    }
    ensure_private_directory(&config.state_directory)?;
    config.state_directory = config
        .state_directory
        .canonicalize()
        .map_err(|_| AgentGatewayError::InvalidStateDirectory(config.state_directory.clone()))?;
    let mut provider_ids = HashSet::new();
    for provider in &mut config.acp_providers {
        if !valid_provider_id(&provider.provider_id)
            || provider.provider_id == "codex"
            || provider.implementation_version.is_empty()
            || !provider_ids.insert(provider.provider_id.clone())
        {
            return Err(AgentGatewayError::InvalidAcpProvider(
                provider.provider_id.clone(),
            ));
        }
        provider.executable = provider
            .executable
            .canonicalize()
            .map_err(|_| AgentGatewayError::InvalidAcpProvider(provider.provider_id.clone()))?;
    }
    if let Some(runtime) = config.genui_runtime.as_mut() {
        if runtime.runtime_version.is_empty() || runtime.compiler_version.is_empty() {
            return Err(AgentGatewayError::InvalidGenUiRuntime(
                "runtime and compiler versions are required".into(),
            ));
        }
        runtime.deno_executable = canonical_runtime_asset(&runtime.deno_executable)?;
        runtime.compiler_script = canonical_runtime_asset(&runtime.compiler_script)?;
        runtime.compiler_wasm = canonical_runtime_asset(&runtime.compiler_wasm)?;
        runtime.preview_shell = canonical_runtime_asset(&runtime.preview_shell)?;
    }

    let preview_shell = config
        .genui_runtime
        .as_ref()
        .map(|runtime| read_preview_shell(&runtime.preview_shell))
        .transpose()?
        .map(Arc::<str>::from);
    let preview_runtime_digest = preview_shell
        .as_deref()
        .map(|shell| Arc::<str>::from(sha256_bytes(shell.as_bytes())));
    let editor_lsp = config
        .genui_runtime
        .as_ref()
        .map(|runtime| {
            let digest = sha256_file(&runtime.deno_executable).map_err(|error| {
                AgentGatewayError::InvalidGenUiRuntime(format!(
                    "cannot digest editor Deno runtime: {error}"
                ))
            })?;
            EditorLspService::new(
                runtime.deno_executable.clone(),
                digest,
                runtime.runtime_version.clone(),
                &config.state_directory,
            )
            .map(Arc::new)
            .map_err(|error| AgentGatewayError::InvalidGenUiRuntime(error.to_string()))
        })
        .transpose()?;
    let artifact_draft_compiler = config
        .genui_runtime
        .as_ref()
        .map(|runtime| ArtifactDraftCompiler::new(runtime, &config.state_directory))
        .transpose()?
        .map(Arc::new);
    let artifact_editor_store = Arc::new(
        ArtifactEditorStore::open(&config.state_directory)
            .map_err(|error| AgentGatewayError::WorkspaceRecovery(error.to_string()))?,
    );
    let session_bindings = Arc::new(
        AgentSessionBindingStore::open(&config.state_directory)
            .map_err(|error| AgentGatewayError::WorkspaceRecovery(error.to_string()))?,
    );
    let artifact_runtime_trace_store = Arc::new(
        ArtifactRuntimeTraceStore::open(&config.state_directory)
            .map_err(|error| AgentGatewayError::WorkspaceRecovery(error.to_string()))?,
    );
    let artifact_visual_quality_store = Arc::new(
        ArtifactVisualQualityStore::open(&config.state_directory)
            .map_err(|error| AgentGatewayError::WorkspaceRecovery(error.to_string()))?,
    );
    let workbench_assets = config
        .workbench_assets
        .take()
        .map(|assets| {
            let canonical = assets
                .canonicalize()
                .map_err(|_| AgentGatewayError::InvalidWorkbenchAssets(assets.clone()))?;
            if !canonical.is_dir() || !canonical.join("index.html").is_file() {
                return Err(AgentGatewayError::InvalidWorkbenchAssets(canonical));
            }
            Ok(Arc::new(canonical))
        })
        .transpose()?;

    let recovery = recover_workspace_transactions(&config.workspace, &config.state_directory)
        .map_err(|error| AgentGatewayError::WorkspaceRecovery(error.to_string()))?;
    let workspace_recovery_block =
        reconcile_workspace_recovery(&config.daemon, &config.state_directory, recovery);
    let local_mcp = Arc::new(
        LocalMcpRuntimeManager::new(
            config.daemon.clone(),
            std::mem::take(&mut config.local_mcp_servers),
        )
        .map_err(|error| AgentGatewayError::InvalidLocalMcpRuntime(error.to_string()))?,
    );

    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let runtime = AgentGatewayRuntime {
        config: Arc::new(config),
        local_mcp,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        session_bindings,
        preview_shell,
        preview_runtime_digest,
        workbench_assets,
        editor_lsp,
        artifact_draft_compiler,
        artifact_editor_store,
        artifact_editor_lock: Arc::new(Mutex::new(())),
        artifact_runtime_trace_store,
        artifact_runtime_trace_lock: Arc::new(Mutex::new(())),
        artifact_visual_quality_store,
        artifact_visual_quality_lock: Arc::new(Mutex::new(())),
        artifact_drafts: Arc::new(Mutex::new(HashMap::new())),
        workspace_applies: Arc::new(Mutex::new(HashMap::new())),
        workspace_recovery_block: Arc::new(Mutex::new(workspace_recovery_block)),
    };
    let router = Router::new()
        .route(
            "/agent/providers",
            get(agent_provider_statuses).post(agent_provider_statuses),
        )
        .route("/agent/attention", get(agent_attention))
        .route(
            "/agent/session",
            get(snapshot_session)
                .post(start_session)
                .delete(close_session),
        )
        .route("/agent/session/restart", post(restart_session))
        .route("/agent/session/turn", post(start_turn))
        .route("/agent/session/cancel", post(cancel_turn))
        .route("/agent/session/stream", get(stream_session))
        .route("/agent/session/config", post(set_session_config))
        .route("/agent/session/permission", post(decide_permission))
        .route(
            "/agent/session/mcp",
            get(local_mcp_status).post(propose_local_mcp_launch),
        )
        .route("/agent/session/mcp/call", post(propose_local_mcp_call))
        .route("/agent/session/tier2", get(tier2_results))
        .route("/agent/session/tier2/preview", post(preview_tier2_result))
        .route("/agent/session/tier2/review", post(propose_tier2_review))
        .route("/agent/session/tier2/discard", post(discard_tier2_result))
        .route(
            "/agent/artifact/{artifact_id}/preview",
            get(preview_artifact),
        )
        .route(
            "/agent/artifact/{artifact_id}/render-payload",
            get(artifact_render_payload),
        )
        .route(
            "/agent/artifact/{artifact_id}/source-map",
            get(artifact_source_map),
        )
        .route("/agent/artifact/{artifact_id}/source", get(artifact_source))
        .route(
            "/agent/artifact/{artifact_id}/editor-state",
            get(artifact_editor_state).put(save_artifact_editor_state),
        )
        .route(
            "/agent/artifact/{artifact_id}/runtime-trace",
            get(artifact_runtime_trace).post(append_artifact_runtime_trace),
        )
        .route(
            "/agent/artifact/{artifact_id}/visual-quality",
            get(artifact_visual_quality).post(submit_artifact_visual_quality),
        )
        .route(
            "/agent/artifact/{artifact_id}/debug-capsule",
            get(artifact_debug_capsule),
        )
        .route("/agent/debug-capsule", get(offline_debug_capsule))
        .route(
            "/agent/artifact/{artifact_id}/history",
            get(artifact_history),
        )
        .route(
            "/agent/artifact/{artifact_id}/history/{revision_id}/source",
            get(artifact_history_source),
        )
        .route(
            "/agent/artifact/{artifact_id}/draft",
            get(artifact_draft_status).post(propose_artifact_draft),
        )
        .route(
            "/agent/artifact/{artifact_id}/workspace-preview",
            post(preview_workspace_apply),
        )
        .route(
            "/agent/artifact/{artifact_id}/workspace-apply",
            get(workspace_apply_status).post(propose_workspace_apply),
        )
        .route(
            "/agent/artifact/{artifact_id}/lsp",
            post(artifact_editor_lsp),
        )
        .route("/agent/workbench", get(workbench_index))
        .route("/agent/workbench/", get(workbench_index))
        .route("/agent/workbench/{*path}", get(workbench_asset))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_EDITOR_LSP_BODY_BYTES,
        ))
        .with_state(runtime.clone());
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_receiver.await;
            })
            .await
    });
    Ok(AgentGatewayHandle {
        address,
        shutdown: Some(shutdown_sender),
        task: Some(task),
        runtime,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AgentStreamStateFrame {
    task_id: TaskId,
    build: AgentBuildIdentity,
    status: AgentStatus,
    turn_id: Option<String>,
    error: Option<String>,
    failure: Option<AgentFailure>,
    history_restored: bool,
    pending_operation_id: Option<OperationId>,
    document_revision: u64,
    capabilities: AgentSessionCapabilities,
    goal: Option<AgentThreadGoal>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentStreamFrame<'a> {
    State {
        #[serde(flatten)]
        state: &'a AgentStreamStateFrame,
    },
    Patch {
        status: AgentStatus,
        patch: &'a BlockPatch,
    },
    Resync {
        status: AgentStatus,
        target_revision: u64,
        reason: &'static str,
    },
}

struct AgentEventStreamState {
    runtime: AgentGatewayRuntime,
    session_id: u16,
    patches: mpsc::Receiver<BlockPatch>,
    previous_state: Vec<u8>,
    first_state: Option<Vec<u8>>,
    refresh: tokio::time::Interval,
}

#[derive(Debug)]
enum AgentStreamError {
    Encode,
    TooLarge,
}

#[derive(Debug)]
enum StartError {
    Unavailable,
    ProviderMismatch,
    Capacity,
    Lock,
    Driver,
}

#[derive(Debug)]
enum SessionError {
    NotFound,
    Busy,
    NoActiveTurn,
    PromptTooLarge,
    InvalidConfig,
    Unsupported,
    Lock,
    Daemon,
    Thread,
    NoPendingEffect,
    StalePermission,
    UnsafeApproval,
    Driver,
    ArtifactUnavailable,
}

#[derive(Debug)]
enum EditorRequestError {
    SessionUnavailable,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    RuntimeUnavailable,
    Driver,
}

#[derive(Debug)]
enum ArtifactDraftError {
    SessionUnavailable,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    NoChanges,
    Busy,
    RuntimeUnavailable,
    Daemon,
    Lock,
}

#[derive(Debug)]
enum ArtifactEditorError {
    SessionUnavailable,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    Lock,
    Store,
}

#[derive(Debug)]
enum RuntimeTraceError {
    SessionUnavailable,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    Sequence,
    Lock,
    Store,
}

#[derive(Debug)]
enum BugCapsuleRequestError {
    SessionUnavailable,
    ArtifactUnavailable,
    Lock,
    Store,
}

#[derive(Debug)]
enum WorkspaceProposalError {
    SessionUnavailable,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    UnsafeTarget,
    NoChanges,
    Busy,
    RecoveryRequired,
    Daemon,
    Lock,
}

fn reconcile_workspace_recovery(
    daemon: &DaemonState,
    state_directory: &Path,
    report: WorkspaceRecoveryReport,
) -> Option<String> {
    let mut blocked = report.blocked;
    for receipt in report.receipts {
        if let Err(error) = reconcile_workspace_receipt(daemon, state_directory, &receipt) {
            blocked.push(format!(
                "workspace transaction {} could not reconcile its operation receipt: {error}",
                receipt.transaction_id
            ));
        }
    }
    if blocked.is_empty() {
        None
    } else {
        Some(bounded_error(&blocked.join("; ")))
    }
}

fn reconcile_workspace_receipt(
    daemon: &DaemonState,
    state_directory: &Path,
    receipt: &WorkspaceTransactionReceipt,
) -> Result<(), String> {
    let operation = daemon
        .operation(receipt.operation_id)
        .map_err(|error| error.to_string())?;
    if operation.task_id != receipt.task_id
        || operation.revision < receipt.operation_revision
        || !matches!(
            &operation.action,
            OperationAction::Opaque { kind, .. } if kind == "hyper_term.workspace.apply"
        )
    {
        return Err("durable receipt does not match the brokered operation".into());
    }
    let expected_terminal = match receipt.outcome {
        WorkspaceTransactionOutcome::Committed => OperationState::Succeeded,
        WorkspaceTransactionOutcome::RolledBack => OperationState::Failed,
    };
    if matches!(
        operation.state,
        OperationState::Dispatching | OperationState::UnknownExecution
    ) {
        let succeeded = receipt.outcome == WorkspaceTransactionOutcome::Committed;
        daemon
            .complete_operation(
                receipt.task_id,
                receipt.operation_id,
                operation.revision,
                OperationCompletion {
                    executor: "hyper-term-workspace-recovery".into(),
                    succeeded,
                    outcome: Some(if succeeded {
                        OperationOutcome::Succeeded
                    } else {
                        OperationOutcome::Failed
                    }),
                    summary: receipt.failure_summary.clone().unwrap_or_else(|| {
                        if succeeded {
                            "recovered a fully committed workspace transaction".into()
                        } else {
                            "recovered and rolled back an interrupted workspace transaction".into()
                        }
                    }),
                    result_digest: succeeded.then(|| receipt.result_digest.clone()),
                },
            )
            .map_err(|error| error.to_string())?;
    } else if operation.state != expected_terminal {
        return Err(format!(
            "operation is {:?}, expected {:?}",
            operation.state, expected_terminal
        ));
    }
    acknowledge_workspace_transaction(state_directory, receipt.transaction_id)
        .map_err(|error| error.to_string())
}

fn acp_network_allowed_hosts(provider_id: &str) -> Option<&'static [&'static str]> {
    match provider_id {
        "codex-acp" => Some(CODEX_NETWORK_ALLOWED_HOSTS),
        "claude-acp" => Some(CLAUDE_NETWORK_ALLOWED_HOSTS),
        "copilot-acp" => Some(COPILOT_NETWORK_ALLOWED_HOSTS),
        _ if cfg!(debug_assertions) => Some(CODEX_NETWORK_ALLOWED_HOSTS),
        _ => None,
    }
}

fn acp_provider_read_paths(provider: &AcpAgentProviderConfig) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let search_paths = provider
        .environment
        .get("PATH")
        .map(|path| std::env::split_paths(path).collect::<Vec<_>>())
        .unwrap_or_default();
    add_acp_runtime_path(&mut paths, &mut seen, &provider.executable, &search_paths);
    for argument in &provider.arguments {
        let path = PathBuf::from(argument);
        if path.is_absolute() {
            add_acp_runtime_path(&mut paths, &mut seen, &path, &search_paths);
        }
    }
    for name in ["CODEX_PATH", "CLAUDE_CODE_EXECUTABLE"] {
        if let Some(value) = provider.environment.get(name) {
            add_acp_runtime_path(&mut paths, &mut seen, Path::new(value), &search_paths);
        }
    }
    for directory in &search_paths {
        add_existing_acp_read_path(&mut paths, &mut seen, directory);
    }
    let Some(home) = provider.environment.get("HOME").map(PathBuf::from) else {
        return paths;
    };
    let credential_paths: &[&str] = match provider.provider_id.as_str() {
        "codex-acp" => &[".codex/auth.json", ".codex/config.toml", ".codex/AGENTS.md"],
        "claude-acp" => &[
            ".claude/settings.json",
            ".claude/settings.local.json",
            ".claude/CLAUDE.md",
            ".claude/skills",
        ],
        "copilot-acp" => &[".config/github-copilot", ".config/gh", "Library/Keychains"],
        _ => &[],
    };
    for relative in credential_paths {
        add_existing_acp_read_path(&mut paths, &mut seen, &home.join(relative));
    }
    paths
}

fn add_acp_runtime_path(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    path: &Path,
    search_paths: &[PathBuf],
) {
    let Ok(canonical) = path.canonicalize() else {
        return;
    };
    add_unique_acp_read_path(paths, seen, canonical.clone());
    if let Some(node_modules) = canonical.ancestors().find(|ancestor| {
        ancestor
            .file_name()
            .is_some_and(|name| name == "node_modules")
    }) {
        add_unique_acp_read_path(paths, seen, node_modules.to_path_buf());
    }
    if let Some(parent) = path.parent() {
        add_existing_acp_read_path(paths, seen, parent);
    }
    add_acp_script_interpreter(paths, seen, &canonical, search_paths);
}

fn add_acp_script_interpreter(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    script: &Path,
    search_paths: &[PathBuf],
) {
    let Ok(mut file) = std::fs::File::open(script) else {
        return;
    };
    let mut buffer = [0_u8; MAX_ACP_SHEBANG_BYTES];
    let Ok(read) = file.read(&mut buffer) else {
        return;
    };
    let Ok(header) = std::str::from_utf8(&buffer[..read]) else {
        return;
    };
    let Some(shebang) = header
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
    else {
        return;
    };
    let words = shebang.split_ascii_whitespace().collect::<Vec<_>>();
    let Some(first) = words.first() else {
        return;
    };
    let command = if Path::new(first)
        .file_name()
        .is_some_and(|name| name == "env")
    {
        words
            .iter()
            .skip(1)
            .copied()
            .find(|word| !word.starts_with('-'))
    } else {
        Some(*first)
    };
    let Some(command) = command else {
        return;
    };
    let command = PathBuf::from(command);
    let interpreter = if command.is_absolute() {
        command.canonicalize().ok()
    } else {
        search_paths
            .iter()
            .map(|directory| directory.join(&command))
            .find_map(|candidate| candidate.canonicalize().ok())
    };
    let Some(interpreter) = interpreter else {
        return;
    };
    add_unique_acp_read_path(paths, seen, interpreter.clone());
    if let Some(install_root) = interpreter.ancestors().find(|ancestor| {
        ancestor
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "Cellar")
    }) {
        add_unique_acp_read_path(paths, seen, install_root.to_path_buf());
    }
    let homebrew = Path::new("/opt/homebrew");
    if interpreter.starts_with(homebrew) {
        add_existing_acp_read_path(paths, seen, homebrew);
    }
}

fn add_existing_acp_read_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: &Path) {
    if let Ok(canonical) = path.canonicalize() {
        add_unique_acp_read_path(paths, seen, canonical);
    }
}

fn add_unique_acp_read_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn agent_diagnostics_enabled() -> bool {
    std::env::var_os("HYPER_TERM_AGENT_DIAGNOSTICS").is_some_and(|value| value == "1")
}

fn bounded_agent_diagnostic(value: &str) -> String {
    const MAX_DIAGNOSTIC_CHARS: usize = 4096;
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}

#[cfg(unix)]
fn latest_claude_debug_tail(session_root: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    const MAX_DEBUG_FILE_BYTES: u64 = 4 * 1024 * 1024;
    const MAX_DEBUG_TAIL_BYTES: usize = 4096;
    let debug_directory = session_root.join("home/.claude/debug");
    let directory_metadata = std::fs::symlink_metadata(&debug_directory).ok()?;
    if directory_metadata.file_type().is_symlink()
        || !directory_metadata.is_dir()
        || directory_metadata.uid() != unsafe { libc::geteuid() }
    {
        return None;
    }
    let path = std::fs::read_dir(debug_directory)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
            (!metadata.file_type().is_symlink()
                && metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.len() <= MAX_DEBUG_FILE_BYTES)
                .then_some((metadata.modified().ok()?, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)?
        .1;
    let bytes = std::fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(MAX_DEBUG_TAIL_BYTES);
    Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

#[cfg(not(unix))]
fn latest_claude_debug_tail(_session_root: &Path) -> Option<String> {
    None
}

impl ArtifactDraftCompiler {
    fn new(
        runtime: &AgentGenUiRuntimeConfig,
        state_directory: &Path,
    ) -> Result<Self, AgentGatewayError> {
        let root = state_directory.join("artifact-drafts");
        create_private_runtime_root(&root)?;
        let cache_directory = root.join("cache");
        let scratch_directory = root.join("scratch");
        create_private_runtime_root(&cache_directory)?;
        create_private_runtime_root(&scratch_directory)?;
        let executable_sha256 = sha256_file(&runtime.deno_executable)
            .map_err(|error| AgentGatewayError::InvalidGenUiRuntime(error.to_string()))?;
        let compiler_script_sha256 = sha256_file(&runtime.compiler_script)
            .map_err(|error| AgentGatewayError::InvalidGenUiRuntime(error.to_string()))?;
        let compiler_wasm_sha256 = sha256_file(&runtime.compiler_wasm)
            .map_err(|error| AgentGatewayError::InvalidGenUiRuntime(error.to_string()))?;
        Ok(Self {
            config: DenoGenUiConfig {
                executable: runtime.deno_executable.clone(),
                executable_sha256,
                runtime_version: runtime.runtime_version.clone(),
                compiler_script: runtime.compiler_script.clone(),
                compiler_script_sha256,
                compiler_wasm: runtime.compiler_wasm.clone(),
                compiler_wasm_sha256,
                compiler_version: runtime.compiler_version.clone(),
                cache_directory,
                scratch_directory,
            },
            compiler: Mutex::new(None),
        })
    }

    fn compile(&self, request: GenUiCompileRequest) -> Result<GenUiArtifactCandidate, String> {
        let compiler = {
            let mut compiler = self
                .compiler
                .lock()
                .map_err(|_| "Artifact compiler lock is poisoned".to_owned())?;
            if compiler
                .as_ref()
                .is_some_and(|compiler| compiler.state().is_ok_and(DriverState::is_terminal))
            {
                compiler.take();
            }
            if compiler.is_none() {
                *compiler = Some(Arc::new(
                    DenoGenUiCompiler::launch(self.config.clone(), Duration::from_secs(10))
                        .map_err(|error| error.to_string())?,
                ));
            }
            Arc::clone(
                compiler
                    .as_ref()
                    .expect("artifact compiler was initialized"),
            )
        };
        let result = compiler
            .compile(request.clone(), Duration::from_secs(15))
            .map_err(|error| error.to_string());
        if compiler.state().is_ok_and(DriverState::is_terminal)
            && let Ok(mut active) = self.compiler.lock()
            && active
                .as_ref()
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &compiler))
        {
            active.take();
        }
        let candidate = result?;
        Ok(GenUiArtifactCandidate {
            schema_version: candidate.schema_version as u16,
            source_revision: candidate.source_revision,
            entrypoint: candidate.entrypoint,
            source_files: request.files,
            bundle: candidate.bundle,
            css: candidate.css,
            source_map: candidate.source_map,
            content_digest: candidate.content_digest,
            compiler: hyper_term_protocol::GenUiCompilerIdentity {
                name: candidate.compiler.name,
                version: candidate.compiler.version,
            },
            diagnostics: candidate
                .diagnostics
                .into_iter()
                .map(|diagnostic| hyper_term_protocol::GenUiCompileDiagnostic {
                    severity: diagnostic.severity,
                    text: diagnostic.text,
                    file: diagnostic.file,
                    line: diagnostic.line,
                    column: diagnostic.column,
                })
                .collect(),
        })
    }

    fn close(&self) {
        if let Ok(mut compiler) = self.compiler.lock()
            && let Some(compiler) = compiler.take()
        {
            let _ = compiler.shutdown();
        }
    }
}

fn validate_artifact_draft(
    artifact: &crate::artifact_store::StoredGenUiArtifact,
    draft: AgentArtifactDraftRequest,
) -> Result<GenUiCompileRequest, ArtifactDraftError> {
    if draft.base_source_revision != artifact.metadata.source_revision {
        return Err(ArtifactDraftError::StaleRevision);
    }
    if draft.entrypoint != artifact.metadata.entrypoint
        || draft.files.is_empty()
        || draft.files.len() > hyper_term_protocol::MAX_GENUI_SOURCE_FILES
        || !draft.files.contains_key(&draft.entrypoint)
        || !draft.files.keys().eq(artifact.source_files.keys())
    {
        return Err(ArtifactDraftError::InvalidRequest);
    }
    let source_bytes = draft
        .files
        .iter()
        .try_fold(0_usize, |total, (path, source)| {
            total.checked_add(path.len())?.checked_add(source.len())
        });
    if source_bytes.is_none_or(|bytes| bytes > hyper_term_protocol::MAX_GENUI_SOURCE_BYTES) {
        return Err(ArtifactDraftError::InvalidRequest);
    }
    if draft.files == artifact.source_files {
        return Err(ArtifactDraftError::NoChanges);
    }
    let source_revision = artifact
        .metadata
        .source_revision
        .checked_add(1)
        .ok_or(ArtifactDraftError::StaleRevision)?;
    Ok(GenUiCompileRequest {
        source_revision,
        entrypoint: draft.entrypoint,
        files: draft.files,
    })
}

fn artifact_draft_response(
    operation_id: OperationId,
    operation_revision: u64,
    record: &ArtifactDraftRecord,
) -> AgentArtifactDraftResponse {
    let (status, artifact, error) = match &record.state {
        ArtifactDraftState::WaitingApproval => (ArtifactDraftStatus::WaitingApproval, None, None),
        ArtifactDraftState::Compiling => (ArtifactDraftStatus::Compiling, None, None),
        ArtifactDraftState::Accepted(artifact) => {
            (ArtifactDraftStatus::Accepted, Some(artifact.clone()), None)
        }
        ArtifactDraftState::Rejected => (ArtifactDraftStatus::Rejected, None, None),
        ArtifactDraftState::Failed(error) => {
            (ArtifactDraftStatus::Failed, None, Some(error.clone()))
        }
    };
    AgentArtifactDraftResponse {
        operation_id,
        operation_revision,
        status,
        artifact,
        error,
    }
}

fn normalize_workspace_apply_mappings(
    request: AgentWorkspaceApplyRequest,
) -> Result<Vec<AgentWorkspaceApplyMapping>, WorkspaceProposalError> {
    let mut mappings = if request.mappings.is_empty() {
        match (request.source_path, request.target_path) {
            (Some(source_path), Some(target_path)) => {
                vec![AgentWorkspaceApplyMapping {
                    source_path,
                    target_path,
                    hunk_ids: Vec::new(),
                }]
            }
            _ => return Err(WorkspaceProposalError::InvalidRequest),
        }
    } else {
        if request.source_path.is_some() || request.target_path.is_some() {
            return Err(WorkspaceProposalError::InvalidRequest);
        }
        request.mappings
    };
    if mappings.is_empty() || mappings.len() > MAX_WORKSPACE_APPLY_FILES {
        return Err(WorkspaceProposalError::InvalidRequest);
    }
    if request
        .review_digest
        .as_deref()
        .is_some_and(|digest| !is_sha256_digest(digest))
    {
        return Err(WorkspaceProposalError::InvalidRequest);
    }
    let mut source_paths = HashSet::new();
    if mappings.iter().any(|mapping| {
        let unique_hunks = mapping.hunk_ids.iter().collect::<HashSet<_>>();
        mapping.source_path.is_empty()
            || mapping.target_path.is_empty()
            || !source_paths.insert(mapping.source_path.clone())
            || mapping.hunk_ids.len() > MAX_WORKSPACE_HUNKS_PER_FILE
            || unique_hunks.len() != mapping.hunk_ids.len()
            || mapping.hunk_ids.iter().any(|hunk| !is_sha256_digest(hunk))
    }) {
        return Err(WorkspaceProposalError::InvalidRequest);
    }
    mappings.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    Ok(mappings)
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn map_workspace_prepare_error(error: WorkspaceApplyError) -> WorkspaceProposalError {
    match error {
        WorkspaceApplyError::NoChanges => WorkspaceProposalError::NoChanges,
        WorkspaceApplyError::InvalidPath
        | WorkspaceApplyError::ParentUnavailable
        | WorkspaceApplyError::ParentChanged
        | WorkspaceApplyError::UnsupportedTarget
        | WorkspaceApplyError::TooLarge
        | WorkspaceApplyError::StaleBase
        | WorkspaceApplyError::UnknownExecution(_)
        | WorkspaceApplyError::RecoveryRequired(_) => WorkspaceProposalError::UnsafeTarget,
        WorkspaceApplyError::Io(_) => WorkspaceProposalError::Daemon,
    }
}

fn map_artifact_editor_store_error(error: ArtifactEditorStoreError) -> ArtifactEditorError {
    match error {
        ArtifactEditorStoreError::StaleRevision { .. }
        | ArtifactEditorStoreError::ContextMismatch
        | ArtifactEditorStoreError::RevisionGap => ArtifactEditorError::StaleRevision,
        ArtifactEditorStoreError::InvalidPath
        | ArtifactEditorStoreError::InvalidFileSet
        | ArtifactEditorStoreError::InvalidEditorState
        | ArtifactEditorStoreError::TooLarge
        | ArtifactEditorStoreError::TornJournal
        | ArtifactEditorStoreError::RevisionOverflow => ArtifactEditorError::InvalidRequest,
        ArtifactEditorStoreError::UnsupportedSchema(_)
        | ArtifactEditorStoreError::Io(_)
        | ArtifactEditorStoreError::Json(_) => ArtifactEditorError::Store,
    }
}

fn map_runtime_trace_store_error(error: RuntimeTraceStoreError) -> RuntimeTraceError {
    match error {
        RuntimeTraceStoreError::ContextMismatch => RuntimeTraceError::StaleRevision,
        RuntimeTraceStoreError::SequenceConflict
        | RuntimeTraceStoreError::SequenceGap { .. }
        | RuntimeTraceStoreError::SequenceOverflow => RuntimeTraceError::Sequence,
        RuntimeTraceStoreError::InvalidPath
        | RuntimeTraceStoreError::InvalidEvent
        | RuntimeTraceStoreError::TornJournal
        | RuntimeTraceStoreError::TooLarge => RuntimeTraceError::InvalidRequest,
        RuntimeTraceStoreError::Clock
        | RuntimeTraceStoreError::UnsupportedStorageSchema(_)
        | RuntimeTraceStoreError::StoredEventDigestMismatch
        | RuntimeTraceStoreError::Io(_)
        | RuntimeTraceStoreError::Json(_) => RuntimeTraceError::Store,
    }
}

fn workspace_preview_response(review: &PreparedWorkspaceReview) -> AgentWorkspacePreviewResponse {
    let changes = review
        .source_paths
        .iter()
        .zip(&review.plan.plans)
        .zip(&review.diffs)
        .map(
            |((source_path, plan), diff)| AgentWorkspacePreviewChangeResponse {
                source_path: source_path.clone(),
                target_path: plan.target_path.clone(),
                base_digest: plan.base_digest().map(str::to_owned),
                artifact_digest: diff.artifact_digest.clone(),
                before: plan.base_content().to_owned(),
                artifact_after: plan.proposed_content.clone(),
                hunks: diff.hunks.clone(),
            },
        )
        .collect();
    AgentWorkspacePreviewResponse {
        artifact_source_revision: review.artifact_source_revision,
        review_digest: review.review_digest.clone(),
        changes,
    }
}

fn workspace_apply_response(
    operation_id: OperationId,
    operation_revision: u64,
    record: &WorkspaceApplyRecord,
) -> AgentWorkspaceApplyResponse {
    let (status, error) = match &record.state {
        WorkspaceApplyState::WaitingApproval => (WorkspaceApplyStatus::WaitingApproval, None),
        WorkspaceApplyState::Applying => (WorkspaceApplyStatus::Applying, None),
        WorkspaceApplyState::Applied => (WorkspaceApplyStatus::Applied, None),
        WorkspaceApplyState::Rejected => (WorkspaceApplyStatus::Rejected, None),
        WorkspaceApplyState::Failed(error) => (WorkspaceApplyStatus::Failed, Some(error.clone())),
        WorkspaceApplyState::UnknownExecution(error) => {
            (WorkspaceApplyStatus::UnknownExecution, Some(error.clone()))
        }
    };
    let changes = record
        .source_paths
        .iter()
        .zip(&record.plan.plans)
        .map(|(source_path, plan)| AgentWorkspaceApplyChangeResponse {
            source_path: source_path.clone(),
            target_path: plan.target_path.clone(),
            base_digest: plan.base_digest().map(str::to_owned),
            proposed_digest: plan.proposed_digest.clone(),
            before: plan.base_content().to_owned(),
            after: plan.proposed_content.clone(),
        })
        .collect::<Vec<_>>();
    let first = changes
        .first()
        .expect("workspace apply records always contain at least one change");
    AgentWorkspaceApplyResponse {
        operation_id,
        operation_revision,
        status,
        artifact_source_revision: record.artifact_source_revision,
        source_path: first.source_path.clone(),
        target_path: first.target_path.clone(),
        base_digest: first.base_digest.clone(),
        proposed_digest: first.proposed_digest.clone(),
        before: first.before.clone(),
        after: first.after.clone(),
        transaction_digest: record.plan.result_digest.clone(),
        changes,
        error,
    }
}

fn tier2_preview_response(preview: IsolatedAcceptancePreview) -> AgentTier2PreviewResponse {
    let mut remaining_patch_bytes = MAX_TIER2_PREVIEW_PATCH_BYTES;
    let mut response_truncated = preview.changes.len() > MAX_TIER2_PREVIEW_CHANGES;
    let mut changes = Vec::new();
    for change in preview.changes.into_iter().take(MAX_TIER2_PREVIEW_CHANGES) {
        let mut change_truncated = false;
        let mut hunks = Vec::new();
        if !change.binary {
            let diff = review_workspace_diff(&change.target_path, &change.before, &change.after);
            let hunk_count = diff.hunks.len();
            for (hunk_index, hunk) in diff.hunks.into_iter().enumerate() {
                let retained_bytes = utf8_prefix_len(&hunk.patch, remaining_patch_bytes);
                let truncated = retained_bytes < hunk.patch.len();
                hunks.push(AgentTier2PreviewHunkResponse {
                    id: hunk.id,
                    base_start: hunk.base_start,
                    base_lines: hunk.base_lines,
                    proposed_start: hunk.proposed_start,
                    proposed_lines: hunk.proposed_lines,
                    patch: hunk.patch[..retained_bytes].to_owned(),
                    truncated,
                });
                remaining_patch_bytes = remaining_patch_bytes.saturating_sub(retained_bytes);
                if truncated || (remaining_patch_bytes == 0 && hunk_index + 1 < hunk_count) {
                    change_truncated = true;
                    response_truncated = true;
                    break;
                }
            }
        }
        changes.push(AgentTier2PreviewChangeResponse {
            target_path: change.target_path,
            base_digest: change.base_digest,
            proposed_digest: change.proposed_digest,
            deleted: change.deleted,
            binary: change.binary,
            base_bytes: change.base_bytes,
            proposed_bytes: change.proposed_bytes,
            hunks,
            truncated: change_truncated,
        });
        if remaining_patch_bytes == 0 {
            response_truncated = true;
            break;
        }
    }
    AgentTier2PreviewResponse {
        source_operation_id: preview.source_operation_id,
        result_digest: preview.result_digest,
        changes,
        truncated: response_truncated,
    }
}

fn tier2_review_response(review: IsolatedAcceptanceReview) -> AgentTier2ReviewResponse {
    let changed_file_count = review.changes.len();
    AgentTier2ReviewResponse {
        source_operation_id: review.source_operation_id,
        operation_id: review.operation.operation_id,
        operation_revision: review.operation.revision,
        state: review.operation.state,
        result_digest: review.result_digest,
        changed_file_count,
    }
}

fn utf8_prefix_len(value: &str, capacity: usize) -> usize {
    let mut end = value.len().min(capacity);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut digest, byte| {
            let _ = write!(digest, "{byte:02x}");
            digest
        })
}

fn canonical_runtime_asset(path: &std::path::Path) -> Result<PathBuf, AgentGatewayError> {
    if !path.is_absolute() {
        return Err(AgentGatewayError::InvalidGenUiRuntime(format!(
            "asset path must be absolute: {}",
            path.display()
        )));
    }
    path.canonicalize().map_err(|error| {
        AgentGatewayError::InvalidGenUiRuntime(format!("{}: {error}", path.display()))
    })
}

fn read_preview_shell(path: &std::path::Path) -> Result<String, AgentGatewayError> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_PREVIEW_SHELL_BYTES {
        return Err(AgentGatewayError::InvalidGenUiRuntime(
            "preview shell is not a bounded regular file".into(),
        ));
    }
    let shell = std::fs::read_to_string(path)?;
    if shell.matches(ARTIFACT_BOOTSTRAP_MARKER).count() != 1
        || !shell.contains("hyper_term_preview_boot")
    {
        return Err(AgentGatewayError::InvalidGenUiRuntime(
            "preview shell is missing its bootstrap contract".into(),
        ));
    }
    Ok(shell)
}

fn parse_artifact_id(value: &str) -> Option<ArtifactId> {
    value.parse::<uuid::Uuid>().ok().map(ArtifactId::from)
}

fn render_preview_document(
    shell: &str,
    artifact: &crate::artifact_store::StoredGenUiArtifact,
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct BootstrapArtifact<'a> {
        artifact_id: String,
        source_revision: u64,
        content_digest: &'a str,
        bundle: &'a str,
        css: &'a str,
        source_map: &'a str,
    }
    let bootstrap = BootstrapArtifact {
        artifact_id: artifact.metadata.artifact_id.to_string(),
        source_revision: artifact.metadata.source_revision,
        content_digest: &artifact.metadata.content_digest,
        bundle: &artifact.bundle,
        css: &artifact.css,
        source_map: &artifact.source_map,
    };
    let json = serde_json::to_string(&bootstrap)?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    Ok(shell.replacen(
        ARTIFACT_BOOTSTRAP_MARKER,
        &format!("<script>globalThis.__HYPER_BOOTSTRAP_ARTIFACT__={json};</script>"),
        1,
    ))
}

fn allowable_brokered_mcp_operation(operation: &hyper_term_core::OperationRecord) -> bool {
    if operation.kind != OperationKind::McpTool || operation.risk != RiskClass::ReadOnly {
        return false;
    }
    let OperationAction::BrokeredMcpToolCall { call } = &operation.action else {
        return false;
    };
    matches!(
        call.tool_name.as_str(),
        "hyper_term.diff.review" | "hyper_term.lsp.query" | "hyper_term.genui.compile"
    )
}

fn ready_response(session_id: u16, session: &AgentSession) -> AgentSessionResponse {
    AgentSessionResponse {
        session_id,
        provider: session.provider_id.clone(),
        protocol: structured_protocol_name(session.protocol).into(),
        status: "ready",
        task_id: session.task_id,
        thread_id: session.thread_id.clone(),
        history_restored: session.history_restored,
    }
}

fn structured_protocol_name(protocol: StructuredAgentProtocol) -> &'static str {
    match protocol {
        StructuredAgentProtocol::Acp => "acp-v1",
        StructuredAgentProtocol::CodexAppServerV2 => "codex-app-server-v2",
        StructuredAgentProtocol::ClaudeStreamJson => "claude-stream-json",
        StructuredAgentProtocol::Mcp20251125 => "mcp-2025-11-25",
    }
}

fn valid_provider_id(provider_id: &str) -> bool {
    !provider_id.is_empty()
        && provider_id.len() <= 64
        && provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn local_mcp_operation_response(
    operation: OperationRecord,
    runtime: Option<LocalMcpServerRuntimeReceipt>,
    receipt: Option<LocalMcpToolCallReceipt>,
    result: Option<serde_json::Value>,
) -> AgentLocalMcpOperationResponse {
    AgentLocalMcpOperationResponse {
        operation_id: operation.operation_id,
        operation_revision: operation.revision,
        state: operation.state,
        runtime,
        receipt,
        result,
    }
}

fn local_mcp_error_response(error: LocalMcpRuntimeError) -> Response {
    match error {
        LocalMcpRuntimeError::UnknownServer | LocalMcpRuntimeError::ServerNotActive => {
            status_response(StatusCode::NOT_FOUND, "Local MCP server is unavailable")
        }
        LocalMcpRuntimeError::ServerAlreadyActive
        | LocalMcpRuntimeError::ServerBusy
        | LocalMcpRuntimeError::PendingLaunchMissing
        | LocalMcpRuntimeError::PendingCallMissing => status_response(
            StatusCode::CONFLICT,
            "Local MCP operation no longer matches the live runtime",
        ),
        LocalMcpRuntimeError::UnsupportedDecision => status_response(
            StatusCode::FORBIDDEN,
            "Local MCP supports only one-time permission decisions",
        ),
        LocalMcpRuntimeError::Tool(_) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Local MCP tool request is invalid or failed",
        ),
        LocalMcpRuntimeError::DuplicateServer(_)
        | LocalMcpRuntimeError::Plan(_)
        | LocalMcpRuntimeError::Client(_)
        | LocalMcpRuntimeError::Daemon(_)
        | LocalMcpRuntimeError::Lock => status_response(
            StatusCode::BAD_GATEWAY,
            "Local MCP runtime could not complete the operation safely",
        ),
    }
}

fn json_response(status: StatusCode, value: &impl Serialize) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => secure_response(status, "application/json; charset=utf-8", Body::from(bytes)),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "agent response serialization failed",
        ),
    }
}

fn status_response(status: StatusCode, message: &'static str) -> Response {
    secure_response(status, "text/plain; charset=utf-8", Body::from(message))
}

fn secure_response(status: StatusCode, content_type: &'static str, body: Body) -> Response {
    secure_response_with_csp(status, content_type, body, AGENT_CSP)
}

fn secure_response_with_csp(
    status: StatusCode,
    content_type: &'static str,
    body: Body,
    csp: &'static str,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(csp));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

fn constant_time_eq(candidate: &[u8], expected: &[u8]) -> bool {
    if candidate.len() != expected.len() {
        return false;
    }
    candidate
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    include!("agent_gateway/tests_core.rs");
    include!("agent_gateway/tests_session.rs");
    include!("agent_gateway/tests_effects.rs");
}
