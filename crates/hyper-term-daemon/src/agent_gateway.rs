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
use crate::editor_lsp::{EditorLspError, EditorLspRequest, EditorLspResponse, EditorLspService};
use crate::network_proxy::ManagedConnectProxy;
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
    BrokeredMcpRuntimeConfig, DaemonError, DaemonState, DenoGenUiMcpExecutorConfig,
    DenoMcpExecutorConfig, IsolatedAcceptancePreview, IsolatedAcceptanceReview,
    LocalMcpRuntimeError, LocalMcpRuntimeManager, UnixServerHandle, spawn_agent_capability_server,
};

mod view_model;
use view_model::*;
mod agent_turn;
use agent_turn::{
    AgentTurnProjection, bounded_error, continue_turn, execute_agent_terminal_create,
    projected_agent_status, run_turn, set_progress_failed,
};
#[cfg(test)]
use agent_turn::{agent_error_summary, retain_terminal_output};
mod launch_provider;
mod mcp_launch;
mod permission;
use permission::decide_permission;
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
    workbench_assets: Option<Arc<PathBuf>>,
    editor_lsp: Option<Arc<EditorLspService>>,
    artifact_draft_compiler: Option<Arc<ArtifactDraftCompiler>>,
    artifact_editor_store: Arc<ArtifactEditorStore>,
    artifact_editor_lock: Arc<Mutex<()>>,
    artifact_runtime_trace_store: Arc<ArtifactRuntimeTraceStore>,
    artifact_runtime_trace_lock: Arc<Mutex<()>>,
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
    _capability_server: Option<UnixServerHandle>,
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
    error: Option<String>,
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
    std::fs::create_dir_all(&config.state_directory)?;
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
        workbench_assets,
        editor_lsp,
        artifact_draft_compiler,
        artifact_editor_store,
        artifact_editor_lock: Arc::new(Mutex::new(())),
        artifact_runtime_trace_store,
        artifact_runtime_trace_lock: Arc::new(Mutex::new(())),
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

async fn agent_provider_statuses(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if let Err(response) = authorize_gateway_token(&runtime, &query) {
        return *response;
    }
    let config = runtime.config.clone();
    match tokio::task::spawn_blocking(move || probe_agent_provider_statuses(&config)).await {
        Ok(statuses) => json_response(StatusCode::OK, &statuses),
        Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent provider readiness could not be refreshed",
        ),
    }
}

async fn agent_attention(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if let Err(response) = authorize_gateway_token(&runtime, &query) {
        return *response;
    }
    match runtime.attention() {
        Ok(sessions) => json_response(StatusCode::OK, &AgentAttentionResponse { sessions }),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent attention projection failed",
        ),
    }
}

async fn workbench_index(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if query.session_id.is_some() {
        let session_id = match authorize(&runtime, &query) {
            Ok(session_id) => session_id,
            Err(response) => return *response,
        };
        if runtime.session(session_id).is_err() {
            return status_response(StatusCode::NOT_FOUND, "Agent session does not exist");
        }
    } else {
        if let Err(response) = authorize_gateway_token(&runtime, &query) {
            return *response;
        }
        if runtime.config.debug_capsule.is_none() {
            return status_response(StatusCode::NOT_FOUND, "Offline Bug Capsule is unavailable");
        }
    }
    serve_workbench_asset(&runtime, Path::new("index.html")).await
}

async fn workbench_asset(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(path): RoutePath<String>,
) -> Response {
    if path.contains('%') {
        return status_response(StatusCode::BAD_REQUEST, "Workbench asset path is invalid");
    }
    let relative = Path::new(&path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return status_response(StatusCode::BAD_REQUEST, "Workbench asset path is invalid");
    }
    serve_workbench_asset(&runtime, relative).await
}

async fn serve_workbench_asset(runtime: &AgentGatewayRuntime, relative: &Path) -> Response {
    let Some(root) = runtime.workbench_assets.as_ref() else {
        return status_response(StatusCode::NOT_FOUND, "Workbench is unavailable");
    };
    let candidate = root.join(relative);
    let Ok(candidate) = candidate.canonicalize() else {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    };
    let Ok(metadata) = std::fs::metadata(&candidate) else {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    };
    if !candidate.starts_with(root.as_ref())
        || !metadata.is_file()
        || metadata.len() > MAX_WORKBENCH_ASSET_BYTES
    {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    }
    let Ok(bytes) = tokio::fs::read(&candidate).await else {
        return status_response(StatusCode::NOT_FOUND, "Workbench asset was not found");
    };
    let content_type = match candidate.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    };
    let csp = if relative == Path::new("genui/preview.html") {
        WORKBENCH_PREVIEW_CSP
    } else {
        WORKBENCH_CSP
    };
    secure_response_with_csp(StatusCode::OK, content_type, Body::from(bytes), csp)
}

async fn start_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let provider = query.provider.unwrap_or_else(|| "codex".into());
    if !valid_provider_id(&provider) {
        return status_response(StatusCode::BAD_REQUEST, "Agent provider id is invalid");
    }
    let result =
        tokio::task::spawn_blocking(move || runtime.start_agent(session_id, &provider)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(error)) => agent_start_error_response(error),
        Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent provider failed to initialize",
        ),
    }
}

fn agent_start_error_response(error: StartError) -> Response {
    match error {
        StartError::Unavailable => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Requested Agent provider is unavailable",
        ),
        StartError::ProviderMismatch => status_response(
            StatusCode::CONFLICT,
            "Agent session already uses a different provider",
        ),
        StartError::Capacity => {
            status_response(StatusCode::TOO_MANY_REQUESTS, "Agent session limit reached")
        }
        StartError::Lock | StartError::Driver => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent provider failed to initialize",
        ),
    }
}

async fn close_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let task_id = match runtime.close_session(session_id, true) {
        Ok(task_id) => task_id,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session history could not be forgotten safely",
            );
        }
    };
    if let Some(task_id) = task_id {
        runtime.local_mcp.close_task(task_id).await;
    }
    secure_response(
        StatusCode::NO_CONTENT,
        "text/plain; charset=utf-8",
        Body::empty(),
    )
}

async fn local_mcp_status(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    let registered = match runtime.local_mcp.registered_servers() {
        Ok(registered) => registered,
        Err(error) => return local_mcp_error_response(error),
    };
    let active = match runtime
        .local_mcp
        .active_server_receipts(session.task_id)
        .await
    {
        Ok(active) => active,
        Err(error) => return local_mcp_error_response(error),
    };
    json_response(
        StatusCode::OK,
        &AgentLocalMcpStatusResponse { registered, active },
    )
}

async fn propose_local_mcp_launch(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    let request = match serde_json::from_slice::<AgentLocalMcpLaunchRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Local MCP launch is invalid"),
    };
    match runtime
        .local_mcp
        .propose_launch(session.task_id, &request.server_id)
    {
        Ok(operation) => json_response(
            StatusCode::ACCEPTED,
            &local_mcp_operation_response(operation, None, None, None),
        ),
        Err(error) => local_mcp_error_response(error),
    }
}

async fn propose_local_mcp_call(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(_) => return status_response(StatusCode::NOT_FOUND, "Agent session does not exist"),
    };
    let request = match serde_json::from_slice::<AgentLocalMcpCallRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Local MCP call is invalid"),
    };
    match runtime.local_mcp.propose_tool_call(
        session.task_id,
        &request.server_id,
        request.tool_name,
        request.arguments,
    ) {
        Ok(operation) => json_response(
            StatusCode::ACCEPTED,
            &local_mcp_operation_response(operation, None, None, None),
        ),
        Err(error) => local_mcp_error_response(error),
    }
}

async fn snapshot_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    match runtime.snapshot(session_id) {
        Ok(snapshot) => json_response(StatusCode::OK, &snapshot),
        Err(SessionError::NotFound) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent session snapshot failed",
        ),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AgentStreamStateFrame {
    status: AgentStatus,
    turn_id: Option<String>,
    error: Option<String>,
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

fn encode_agent_stream_line(value: &impl Serialize) -> Result<Vec<u8>, AgentStreamError> {
    let mut line = serde_json::to_vec(value).map_err(|_| AgentStreamError::Encode)?;
    if line.len() + 1 > MAX_AGENT_STREAM_LINE_BYTES {
        return Err(AgentStreamError::TooLarge);
    }
    line.push(b'\n');
    Ok(line)
}

async fn stream_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let session = match runtime.session(session_id) {
        Ok(session) => session,
        Err(SessionError::NotFound) => {
            return status_response(StatusCode::NOT_FOUND, "Agent session does not exist");
        }
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session stream could not start",
            );
        }
    };
    let task_id = session.task_id;
    let block_patches = match runtime.config.daemon.subscribe_block_patches() {
        Ok(receiver) => receiver,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session stream could not subscribe",
            );
        }
    };
    let initial_state = match runtime.stream_state(session_id) {
        Ok(state) => state,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session stream could not start",
            );
        }
    };
    let initial = match encode_agent_stream_line(&AgentStreamFrame::State {
        state: &initial_state,
    }) {
        Ok(line) => line,
        Err(_) => {
            return status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Agent session state exceeds the stream frame bound",
            );
        }
    };
    let (patch_sender, patch_receiver) = mpsc::channel(AGENT_STREAM_PATCH_QUEUE);
    let _ = std::thread::Builder::new()
        .name(format!("hyper-term-agent-stream-{session_id}"))
        .spawn(move || {
            while !patch_sender.is_closed() {
                match block_patches.recv_timeout(Duration::from_millis(100)) {
                    Ok((candidate_task_id, patch)) => {
                        if candidate_task_id == task_id
                            && patch_sender.blocking_send(patch).is_err()
                        {
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    let mut refresh = tokio::time::interval(AGENT_STREAM_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let state = AgentEventStreamState {
        runtime,
        session_id,
        patches: patch_receiver,
        previous_state: initial.clone(),
        first_state: Some(initial),
        refresh,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        if let Some(first) = state.first_state.take() {
            return Some((Ok::<Bytes, Infallible>(Bytes::from(first)), state));
        }
        loop {
            tokio::select! {
                patch = state.patches.recv() => {
                    let mut patch = patch?;
                    let mut patch_gap = false;
                    let cadence = tokio::time::sleep(AGENT_STREAM_FRAME_CADENCE);
                    tokio::pin!(cadence);
                    loop {
                        tokio::select! {
                            _ = &mut cadence => break,
                            next = state.patches.recv() => {
                                let Some(next) = next else { break };
                                if next.base_revision != patch.target_revision {
                                    patch.stream_sequence = next.stream_sequence;
                                    patch.target_revision = next.target_revision;
                                    patch_gap = true;
                                    break;
                                }
                                patch.stream_sequence = next.stream_sequence;
                                patch.target_revision = next.target_revision;
                                patch.operations.extend(next.operations);
                            }
                        }
                    }
                    let status = state.runtime.stream_status(state.session_id).ok()?;
                    let frame = if patch_gap {
                        AgentStreamFrame::Resync {
                            status,
                            target_revision: patch.target_revision,
                            reason: "patch_sequence_gap",
                        }
                    } else {
                        AgentStreamFrame::Patch {
                            status,
                            patch: &patch,
                        }
                    };
                    let line = match encode_agent_stream_line(&frame) {
                        Ok(line) => line,
                        Err(AgentStreamError::TooLarge) => encode_agent_stream_line(
                            &AgentStreamFrame::Resync {
                                status,
                                target_revision: patch.target_revision,
                                reason: "patch_frame_too_large",
                            },
                        ).ok()?,
                        Err(AgentStreamError::Encode) => return None,
                    };
                    return Some((Ok(Bytes::from(line)), state));
                }
                _ = state.refresh.tick() => {
                    let current = state.runtime.stream_state(state.session_id).ok()?;
                    let line = encode_agent_stream_line(&AgentStreamFrame::State {
                        state: &current,
                    }).ok()?;
                    if line == state.previous_state {
                        continue;
                    }
                    state.previous_state = line.clone();
                    return Some((Ok(Bytes::from(line)), state));
                }
            }
        }
    });
    secure_response(
        StatusCode::OK,
        "application/x-ndjson; charset=utf-8",
        Body::from_stream(stream),
    )
}

async fn preview_artifact(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.preview_document(session_id, artifact_id) {
        Ok(document) => secure_response_with_csp(
            StatusCode::OK,
            "text/html; charset=utf-8",
            Body::from(document),
            PREVIEW_CSP,
        ),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact preview is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact preview could not be rendered",
        ),
    }
}

async fn artifact_source_map(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.artifact_source_map(session_id, artifact_id) {
        Ok(source_map) => secure_response(
            StatusCode::OK,
            "application/json; charset=utf-8",
            Body::from(source_map),
        ),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source map is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact source map could not be read",
        ),
    }
}

async fn artifact_source(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.artifact_source(session_id, artifact_id) {
        Ok(source) => json_response(StatusCode::OK, &source),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact source could not be read",
        ),
    }
}

async fn artifact_editor_state(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let result =
        tokio::task::spawn_blocking(move || runtime.artifact_editor_state(session_id, artifact_id))
            .await;
    artifact_editor_response(result)
}

async fn save_artifact_editor_state(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<ArtifactEditorCheckpointRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Artifact editor checkpoint is invalid",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.save_artifact_editor_state(session_id, artifact_id, request)
    })
    .await;
    artifact_editor_response(result)
}

fn artifact_editor_response(
    result: Result<Result<ArtifactEditorCheckpoint, ArtifactEditorError>, tokio::task::JoinError>,
) -> Response {
    match result {
        Ok(Ok(checkpoint)) => json_response(StatusCode::OK, &checkpoint),
        Ok(Err(
            ArtifactEditorError::SessionUnavailable | ArtifactEditorError::ArtifactUnavailable,
        )) => status_response(
            StatusCode::NOT_FOUND,
            "Artifact editor state is unavailable",
        ),
        Ok(Err(ArtifactEditorError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Artifact editor state is available only for ACP Agent artifacts",
        ),
        Ok(Err(ArtifactEditorError::StaleRevision)) => status_response(
            StatusCode::CONFLICT,
            "Artifact editor checkpoint no longer matches the current revision",
        ),
        Ok(Err(ArtifactEditorError::InvalidRequest)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Artifact editor checkpoint violates the bounded fixed-path state",
        ),
        Ok(Err(ArtifactEditorError::Lock | ArtifactEditorError::Store)) | Err(_) => {
            status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Artifact editor checkpoint could not be persisted",
            )
        }
    }
}

async fn artifact_runtime_trace(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.artifact_runtime_trace(session_id, artifact_id)
    })
    .await;
    artifact_runtime_trace_response(result)
}

async fn append_artifact_runtime_trace(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<GenUiRuntimeTraceAppendRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(StatusCode::BAD_REQUEST, "Runtime trace batch is invalid");
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.append_artifact_runtime_trace(session_id, artifact_id, request)
    })
    .await;
    artifact_runtime_trace_response(result)
}

fn artifact_runtime_trace_response(
    result: Result<Result<GenUiRuntimeTraceProjection, RuntimeTraceError>, tokio::task::JoinError>,
) -> Response {
    match result {
        Ok(Ok(projection)) => json_response(StatusCode::OK, &projection),
        Ok(Err(RuntimeTraceError::SessionUnavailable | RuntimeTraceError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Runtime trace is unavailable")
        }
        Ok(Err(RuntimeTraceError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Runtime trace is available only for ACP Agent artifacts",
        ),
        Ok(Err(RuntimeTraceError::StaleRevision | RuntimeTraceError::Sequence)) => status_response(
            StatusCode::CONFLICT,
            "Runtime trace no longer matches the current Artifact stream",
        ),
        Ok(Err(RuntimeTraceError::InvalidRequest)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Runtime trace violates the bounded redacted event contract",
        ),
        Ok(Err(RuntimeTraceError::Lock | RuntimeTraceError::Store)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Runtime trace could not be persisted",
        ),
    }
}

async fn artifact_debug_capsule(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.artifact_debug_capsule(session_id, artifact_id)
    })
    .await;
    match result {
        Ok(Ok(capsule)) => json_response(StatusCode::OK, &capsule),
        Ok(Err(
            BugCapsuleRequestError::SessionUnavailable
            | BugCapsuleRequestError::ArtifactUnavailable,
        )) => status_response(StatusCode::NOT_FOUND, "Bug capsule is unavailable"),
        Ok(Err(BugCapsuleRequestError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Bug capsules are available only for ACP Agent artifacts",
        ),
        Ok(Err(BugCapsuleRequestError::Lock | BugCapsuleRequestError::Store)) | Err(_) => {
            status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Bug capsule could not be prepared",
            )
        }
    }
}

async fn offline_debug_capsule(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    if let Err(response) = authorize_gateway_token(&runtime, &query) {
        return *response;
    }
    match runtime.config.debug_capsule.as_ref() {
        Some(capsule) => json_response(StatusCode::OK, capsule),
        None => status_response(StatusCode::NOT_FOUND, "Offline Bug Capsule is unavailable"),
    }
}

async fn artifact_history(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    match runtime.artifact_history(session_id, artifact_id) {
        Ok(history) => json_response(StatusCode::OK, &history),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact history is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact history could not be read",
        ),
    }
}

async fn artifact_history_source(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath((artifact_id, revision_id)): RoutePath<(String, String)>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let Some(artifact_id) = parse_artifact_id(&artifact_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid");
    };
    let Some(revision_id) = parse_artifact_id(&revision_id) else {
        return status_response(StatusCode::BAD_REQUEST, "Revision id is invalid");
    };
    match runtime.artifact_history_source(session_id, artifact_id, revision_id) {
        Ok(source) => json_response(StatusCode::OK, &source),
        Err(SessionError::NotFound | SessionError::ArtifactUnavailable) => status_response(
            StatusCode::NOT_FOUND,
            "Artifact revision source is unavailable",
        ),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact revision source could not be read",
        ),
    }
}

async fn artifact_editor_lsp(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<EditorLspRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(StatusCode::BAD_REQUEST, "Editor LSP request is invalid");
        }
    };
    if request.validate().is_err() {
        return status_response(StatusCode::BAD_REQUEST, "Editor LSP request is invalid");
    }
    match tokio::task::spawn_blocking(move || {
        runtime.editor_lsp_query(session_id, artifact_id, request)
    })
    .await
    {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(EditorRequestError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(EditorRequestError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Editor LSP is available only for ACP Agent artifacts",
        ),
        Ok(Err(EditorRequestError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(EditorRequestError::StaleRevision)) => status_response(
            StatusCode::CONFLICT,
            "Editor source revision is no longer current",
        ),
        Ok(Err(EditorRequestError::InvalidRequest)) => {
            status_response(StatusCode::BAD_REQUEST, "Editor LSP request is invalid")
        }
        Ok(Err(EditorRequestError::RuntimeUnavailable)) => {
            status_response(StatusCode::SERVICE_UNAVAILABLE, "Editor LSP is unavailable")
        }
        Ok(Err(EditorRequestError::Driver)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Editor LSP request could not be completed",
        ),
    }
}

async fn propose_artifact_draft(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<AgentArtifactDraftRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Artifact draft is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.propose_artifact_draft(session_id, artifact_id, request)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(ArtifactDraftError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(ArtifactDraftError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Artifact publishing is available only for ACP Agent artifacts",
        ),
        Ok(Err(ArtifactDraftError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(ArtifactDraftError::StaleRevision | ArtifactDraftError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Artifact draft no longer matches the current revision",
        ),
        Ok(Err(ArtifactDraftError::InvalidRequest)) => {
            status_response(StatusCode::BAD_REQUEST, "Artifact draft is invalid")
        }
        Ok(Err(ArtifactDraftError::NoChanges)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Artifact draft has no changes",
        ),
        Ok(Err(ArtifactDraftError::RuntimeUnavailable)) => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Rust-supervised Deno artifact publishing is unavailable",
        ),
        Ok(Err(ArtifactDraftError::Daemon | ArtifactDraftError::Lock)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact draft could not enter the permission broker",
        ),
    }
}

async fn artifact_draft_status(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentArtifactDraftStatusQuery>,
) -> Response {
    let session_id = match authorize(
        &runtime,
        &AgentSessionQuery {
            token: query.token,
            session_id: query.session_id,
            provider: None,
        },
    ) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let Some(operation_id) = query.operation_id else {
        return status_response(StatusCode::BAD_REQUEST, "Draft operation id is invalid");
    };
    match runtime.artifact_draft_status(session_id, artifact_id, operation_id) {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(ArtifactDraftError::SessionUnavailable | ArtifactDraftError::ArtifactUnavailable) => {
            status_response(StatusCode::NOT_FOUND, "Artifact draft is unavailable")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Artifact draft status could not be read",
        ),
    }
}

async fn propose_workspace_apply(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<AgentWorkspaceApplyRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Workspace apply request is invalid",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.propose_workspace_apply(session_id, artifact_id, request)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(WorkspaceProposalError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(WorkspaceProposalError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Workspace apply is available only for ACP Agent artifacts",
        ),
        Ok(Err(WorkspaceProposalError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(WorkspaceProposalError::StaleRevision | WorkspaceProposalError::Busy)) => {
            status_response(
                StatusCode::CONFLICT,
                "Workspace apply no longer matches the current revision",
            )
        }
        Ok(Err(WorkspaceProposalError::RecoveryRequired)) => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Workspace apply is blocked until an interrupted transaction is recovered",
        ),
        Ok(Err(WorkspaceProposalError::NoChanges)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Workspace target already matches the artifact source",
        ),
        Ok(Err(WorkspaceProposalError::InvalidRequest | WorkspaceProposalError::UnsafeTarget)) => {
            status_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Workspace target is not a bounded regular file path",
            )
        }
        Ok(Err(WorkspaceProposalError::Daemon | WorkspaceProposalError::Lock)) | Err(_) => {
            status_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Workspace apply could not enter the permission broker",
            )
        }
    }
}

async fn preview_workspace_apply(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let request = match serde_json::from_slice::<AgentWorkspaceApplyRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(
                StatusCode::BAD_REQUEST,
                "Workspace preview request is invalid",
            );
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.preview_workspace_apply(session_id, artifact_id, request)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(WorkspaceProposalError::SessionUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(WorkspaceProposalError::AcpRequired)) => status_response(
            StatusCode::FORBIDDEN,
            "Workspace preview is available only for ACP Agent artifacts",
        ),
        Ok(Err(WorkspaceProposalError::ArtifactUnavailable)) => {
            status_response(StatusCode::NOT_FOUND, "Artifact source is unavailable")
        }
        Ok(Err(WorkspaceProposalError::StaleRevision)) => status_response(
            StatusCode::CONFLICT,
            "Workspace preview no longer matches the current revision",
        ),
        Ok(Err(WorkspaceProposalError::NoChanges)) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Workspace targets already match the artifact source",
        ),
        Ok(Err(WorkspaceProposalError::InvalidRequest | WorkspaceProposalError::UnsafeTarget)) => {
            status_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Workspace targets are not bounded regular file paths",
            )
        }
        Ok(Err(
            WorkspaceProposalError::Busy
            | WorkspaceProposalError::RecoveryRequired
            | WorkspaceProposalError::Daemon
            | WorkspaceProposalError::Lock,
        ))
        | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Workspace preview could not be prepared",
        ),
    }
}

async fn workspace_apply_status(
    State(runtime): State<AgentGatewayRuntime>,
    RoutePath(artifact_id): RoutePath<String>,
    Query(query): Query<AgentWorkspaceApplyStatusQuery>,
) -> Response {
    let session_id = match authorize(
        &runtime,
        &AgentSessionQuery {
            token: query.token,
            session_id: query.session_id,
            provider: None,
        },
    ) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let artifact_id = match parse_artifact_id(&artifact_id) {
        Some(artifact_id) => artifact_id,
        None => return status_response(StatusCode::BAD_REQUEST, "Artifact id is invalid"),
    };
    let Some(operation_id) = query.operation_id else {
        return status_response(
            StatusCode::BAD_REQUEST,
            "Workspace apply operation id is invalid",
        );
    };
    match runtime.workspace_apply_status(session_id, artifact_id, operation_id) {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(
            WorkspaceProposalError::SessionUnavailable
            | WorkspaceProposalError::ArtifactUnavailable,
        ) => status_response(StatusCode::NOT_FOUND, "Workspace apply is unavailable"),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Workspace apply status could not be read",
        ),
    }
}

async fn start_turn(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let prompt = match String::from_utf8(body.to_vec()) {
        Ok(prompt) if !prompt.trim().is_empty() => prompt,
        _ => return status_response(StatusCode::BAD_REQUEST, "Agent prompt is invalid"),
    };
    let is_goal_command = prompt.trim() == "/goal" || prompt.trim().starts_with("/goal ");
    let result = if is_goal_command {
        tokio::task::spawn_blocking(move || runtime.apply_goal_command(session_id, &prompt))
            .await
            .map_err(|_| SessionError::Thread)
            .and_then(|result| result)
    } else {
        runtime.submit_turn(session_id, prompt)
    };
    match result {
        Ok(response) => json_response(StatusCode::ACCEPTED, &response),
        Err(SessionError::NotFound) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Err(SessionError::Busy) => status_response(
            StatusCode::CONFLICT,
            "Agent session already has an active turn",
        ),
        Err(SessionError::PromptTooLarge) => status_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Agent prompt exceeds its bound",
        ),
        Err(SessionError::Unsupported | SessionError::InvalidConfig) => status_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Agent provider does not support this goal command",
        ),
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent turn could not be started",
        ),
    }
}

async fn cancel_turn(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let result = tokio::task::spawn_blocking(move || runtime.cancel_turn(session_id)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(SessionError::NoActiveTurn)) => {
            status_response(StatusCode::CONFLICT, "Agent session has no active turn")
        }
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent turn cancellation could not be delivered safely",
        ),
    }
}

async fn tier2_results(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    match runtime.tier2_results(session_id) {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(SessionError::NotFound) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 review results could not be read",
        ),
    }
}

async fn preview_tier2_result(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentTier2SourceRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Tier 2 result is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.preview_tier2_result(session_id, request.source_operation_id)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Tier 2 result is unavailable")
        }
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 Diff could not be prepared safely",
        ),
    }
}

async fn propose_tier2_review(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentTier2SourceRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Tier 2 result is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.propose_tier2_review(session_id, request.source_operation_id)
    })
    .await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Tier 2 result is unavailable")
        }
        Ok(Err(SessionError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Tier 2 result already has a pending review",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 review could not enter the permission broker",
        ),
    }
}

async fn discard_tier2_result(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentTier2SourceRequest>(&body) {
        Ok(request) => request,
        Err(_) => return status_response(StatusCode::BAD_REQUEST, "Tier 2 result is invalid"),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.discard_tier2_result(session_id, request.source_operation_id)
    })
    .await;
    match result {
        Ok(Ok(())) => secure_response(
            StatusCode::NO_CONTENT,
            "text/plain; charset=utf-8",
            Body::empty(),
        ),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Tier 2 result is unavailable")
        }
        Ok(Err(SessionError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Reject the pending Tier 2 review before discarding its result",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Tier 2 result could not be discarded",
        ),
    }
}

async fn set_session_config(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentConfigRequest>(&body) {
        Ok(request) if !request.config_id.is_empty() && request.config_id.len() <= 128 => request,
        _ => return status_response(StatusCode::BAD_REQUEST, "Agent configuration is invalid"),
    };
    let result =
        tokio::task::spawn_blocking(move || runtime.set_session_config(session_id, request)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(SessionError::Busy)) => status_response(
            StatusCode::CONFLICT,
            "Agent configuration cannot change during an active turn",
        ),
        Ok(Err(SessionError::Unsupported)) => status_response(
            StatusCode::CONFLICT,
            "Agent provider does not expose session configuration",
        ),
        Ok(Err(SessionError::InvalidConfig)) => status_response(
            StatusCode::BAD_REQUEST,
            "Agent configuration value is invalid",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Agent configuration could not be updated",
        ),
    }
}

fn authorize(
    runtime: &AgentGatewayRuntime,
    query: &AgentSessionQuery,
) -> Result<u16, Box<Response>> {
    authorize_gateway_token(runtime, query)?;
    let Some(session_id @ 1..=999) = query.session_id else {
        return Err(Box::new(status_response(
            StatusCode::BAD_REQUEST,
            "agent session id is invalid",
        )));
    };
    Ok(session_id)
}

fn authorize_gateway_token(
    runtime: &AgentGatewayRuntime,
    query: &AgentSessionQuery,
) -> Result<(), Box<Response>> {
    if !constant_time_eq(
        query.token.as_deref().unwrap_or_default().as_bytes(),
        runtime.config.token.as_bytes(),
    ) {
        return Err(Box::new(status_response(
            StatusCode::UNAUTHORIZED,
            "agent gateway token is invalid",
        )));
    }
    Ok(())
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
    AcpRequired,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    RuntimeUnavailable,
    Driver,
}

#[derive(Debug)]
enum ArtifactDraftError {
    SessionUnavailable,
    AcpRequired,
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
    AcpRequired,
    ArtifactUnavailable,
    StaleRevision,
    InvalidRequest,
    Lock,
    Store,
}

#[derive(Debug)]
enum RuntimeTraceError {
    SessionUnavailable,
    AcpRequired,
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
    AcpRequired,
    ArtifactUnavailable,
    Lock,
    Store,
}

#[derive(Debug)]
enum WorkspaceProposalError {
    SessionUnavailable,
    AcpRequired,
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
            ".claude",
            ".claude.json",
            ".config/claude",
            "Library/Keychains",
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

impl AgentGatewayRuntime {
    fn start_agent(
        &self,
        session_id: u16,
        provider_id: &str,
    ) -> Result<AgentSessionResponse, StartError> {
        let mut sessions = self.sessions.lock().map_err(|_| StartError::Lock)?;
        if let Some(session) = sessions.get(&session_id) {
            if session.provider_id != provider_id {
                return Err(StartError::ProviderMismatch);
            }
            return match session.client.state().map_err(|_| StartError::Driver)? {
                DriverState::Ready | DriverState::Busy | DriverState::Waiting => {
                    Ok(ready_response(session_id, session))
                }
                _ => Err(StartError::Driver),
            };
        }
        if sessions.len() >= MAX_AGENT_SESSIONS {
            return Err(StartError::Capacity);
        }
        // A staged Codex auth candidate is the desktop contract and must pass
        // the read-only readiness gate. Library callers that intentionally do
        // not stage credentials may still let app-server negotiate its own
        // authentication. ACP registrations always use the explicit provider
        // readiness contract.
        let enforce_readiness = provider_id != "codex" || self.config.codex_auth_file.is_some();
        if enforce_readiness
            && let Some(status) = probe_known_agent_provider(&self.config, provider_id)
            && !status.usable()
        {
            return Err(StartError::Unavailable);
        }
        let restored_task_id = self
            .session_bindings
            .task_for(session_id, provider_id)
            .map_err(|_| StartError::Driver)?
            .filter(|task_id| self.config.daemon.block_snapshot(*task_id).is_ok());
        let task_id = match restored_task_id {
            Some(task_id) => task_id,
            None => self
                .config
                .daemon
                .create_task(format!("{provider_id} Agent session {session_id}"))
                .map_err(|_| StartError::Driver)?,
        };
        let history_restored = restored_task_id
            .and_then(|task_id| self.config.daemon.block_snapshot(task_id).ok())
            .is_some_and(|document| {
                document
                    .blocks
                    .iter()
                    .any(|block| block.kind != BlockKind::Task)
            });
        let session_root = self
            .config
            .state_directory
            .join("agents")
            .join(format!("session-{session_id}-{task_id}"));
        create_private_runtime_root(&session_root).map_err(|_| StartError::Driver)?;
        let mcp = match self.mcp_launch(task_id, &session_root).transpose() {
            Ok(mcp) => mcp,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(error);
            }
        };
        let launched = match self.launch_provider(provider_id, &session_root, mcp) {
            Ok(launched) => launched,
            Err(error) => {
                self.cleanup_brokered_mcp_runtime(task_id);
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(error);
            }
        };
        let (protocol, timeout) = (launched.client.protocol(), startup::timeout(provider_id));
        let thread_id = match launched.client.initialize_session(timeout) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                if agent_diagnostics_enabled() {
                    let stderr = launched.client.stderr_tail().unwrap_or_default();
                    eprintln!(
                        "hyper-term-agent: {provider_id} initialization failed: {}; stderr={}",
                        bounded_agent_diagnostic(&error.to_string()),
                        bounded_agent_diagnostic(&stderr),
                    );
                    if provider_id == "claude-acp"
                        && let Some(debug) = latest_claude_debug_tail(&session_root)
                    {
                        eprintln!(
                            "hyper-term-agent: claude-acp SDK debug tail={}",
                            bounded_agent_diagnostic(&debug),
                        );
                    }
                }
                let _ = launched.client.close();
                self.cleanup_brokered_mcp_runtime(task_id);
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(StartError::Driver);
            }
        };
        let context_receipts = launched.client.execution_context_receipts();
        if !context_receipts.is_empty()
            && self
                .config
                .daemon
                .record_agent_execution_context(
                    task_id,
                    provider_id.to_owned(),
                    structured_protocol_name(protocol).into(),
                    thread_id.clone(),
                    context_receipts,
                )
                .is_err()
        {
            let _ = launched.client.close();
            self.cleanup_brokered_mcp_runtime(task_id);
            let _ = std::fs::remove_dir_all(&session_root);
            return Err(StartError::Driver);
        }
        let session = Arc::new(AgentSession {
            client: launched.client,
            provider_id: provider_id.to_owned(),
            protocol,
            task_id,
            thread_id,
            history_restored,
            runtime_root: session_root,
            progress: Mutex::new(AgentProgress {
                status: AgentStatus::Ready,
                turn_id: None,
                error: None,
            }),
            pending_effect: Mutex::new(None),
            terminals: Mutex::new(HashMap::new()),
            _managed_proxy: launched.managed_proxy,
            _capability_server: launched.capability_server,
        });
        if self
            .session_bindings
            .bind(session_id, provider_id, task_id)
            .is_err()
        {
            let _ = session.client.close();
            self.cleanup_brokered_mcp_runtime(task_id);
            let _ = std::fs::remove_dir_all(&session.runtime_root);
            return Err(StartError::Driver);
        }
        let response = ready_response(session_id, &session);
        sessions.insert(session_id, session);
        Ok(response)
    }

    fn snapshot(&self, session_id: u16) -> Result<AgentSnapshotResponse, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        let progress_status = progress.status;
        let turn_id = progress.turn_id.clone();
        let error = progress.error.clone();
        drop(progress);
        let pending_operation_id = self.pending_agent_operation(&session)?;
        let status = projected_agent_status(progress_status, pending_operation_id);
        let document = self
            .config
            .daemon
            .block_snapshot(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        let capabilities = session
            .client
            .session_capabilities()
            .map_err(|_| SessionError::Driver)?;
        let goal = session
            .client
            .thread_goal()
            .map_err(|_| SessionError::Driver)?;
        let context = self
            .config
            .daemon
            .agent_execution_context_event(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        Ok(AgentSnapshotResponse {
            session_id,
            status,
            turn_id,
            error,
            history_restored: session.history_restored,
            pending_operation_id,
            capabilities,
            goal,
            context,
            document,
        })
    }

    fn attention(&self) -> Result<Vec<AgentAttentionSession>, SessionError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| SessionError::Lock)?
            .iter()
            .map(|(session_id, session)| (*session_id, Arc::clone(session)))
            .collect::<Vec<_>>();
        sessions.sort_by_key(|(session_id, _)| *session_id);
        sessions
            .into_iter()
            .map(|(session_id, session)| {
                let progress_status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                let pending_operation_id = self.pending_agent_operation(&session)?;
                let status = projected_agent_status(progress_status, pending_operation_id);
                let document_revision = self
                    .config
                    .daemon
                    .block_revision(session.task_id)
                    .map_err(|_| SessionError::Daemon)?;
                Ok(AgentAttentionSession {
                    session_id,
                    provider: session.provider_id.clone(),
                    status,
                    document_revision,
                })
            })
            .collect()
    }

    fn stream_status(&self, session_id: u16) -> Result<AgentStatus, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        Ok(progress.status)
    }

    fn stream_state(&self, session_id: u16) -> Result<AgentStreamStateFrame, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        let progress_status = progress.status;
        let turn_id = progress.turn_id.clone();
        let error = progress.error.clone();
        drop(progress);
        let pending_operation_id = self.pending_agent_operation(&session)?;
        let status = projected_agent_status(progress_status, pending_operation_id);
        let document_revision = self
            .config
            .daemon
            .block_revision(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        let capabilities = session
            .client
            .session_capabilities()
            .map_err(|_| SessionError::Driver)?;
        let goal = session
            .client
            .thread_goal()
            .map_err(|_| SessionError::Driver)?;
        Ok(AgentStreamStateFrame {
            status,
            turn_id,
            error,
            history_restored: session.history_restored,
            pending_operation_id,
            document_revision,
            capabilities,
            goal,
        })
    }

    fn pending_agent_operation(
        &self,
        session: &AgentSession,
    ) -> Result<Option<OperationId>, SessionError> {
        if let Some(operation_id) = session
            .pending_effect
            .lock()
            .map_err(|_| SessionError::Lock)?
            .as_ref()
            .map(|effect| effect.operation_id)
        {
            return Ok(Some(operation_id));
        }
        self.config
            .daemon
            .pending_operation_id(session.task_id)
            .map_err(|_| SessionError::Daemon)
    }

    fn preview_document(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<String, SessionError> {
        let session = self.session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?;
        let shell = self
            .preview_shell
            .as_deref()
            .ok_or(SessionError::ArtifactUnavailable)?;
        render_preview_document(shell, &artifact).map_err(|_| SessionError::ArtifactUnavailable)
    }

    fn artifact_source_map(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<String, SessionError> {
        let session = self.session(session_id)?;
        self.config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map(|artifact| artifact.source_map)
            .map_err(|_| SessionError::ArtifactUnavailable)
    }

    fn artifact_source(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<AgentArtifactSourceResponse, SessionError> {
        let session = self.session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?;
        if artifact.source_files.is_empty() {
            return Err(SessionError::ArtifactUnavailable);
        }
        Ok(AgentArtifactSourceResponse {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            entrypoint: artifact.metadata.entrypoint,
            files: artifact.source_files,
        })
    }

    fn artifact_editor_state(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<ArtifactEditorCheckpoint, ArtifactEditorError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactEditorError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(ArtifactEditorError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| ArtifactEditorError::ArtifactUnavailable)?;
        let _guard = self
            .artifact_editor_lock
            .lock()
            .map_err(|_| ArtifactEditorError::Lock)?;
        self.artifact_editor_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
                &artifact.metadata.entrypoint,
                &artifact.source_files,
            )
            .map_err(map_artifact_editor_store_error)
    }

    fn save_artifact_editor_state(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: ArtifactEditorCheckpointRequest,
    ) -> Result<ArtifactEditorCheckpoint, ArtifactEditorError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactEditorError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(ArtifactEditorError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| ArtifactEditorError::ArtifactUnavailable)?;
        if request.base_source_revision != artifact.metadata.source_revision {
            return Err(ArtifactEditorError::StaleRevision);
        }
        let _guard = self
            .artifact_editor_lock
            .lock()
            .map_err(|_| ArtifactEditorError::Lock)?;
        self.artifact_editor_store
            .save(
                session.task_id,
                artifact_id,
                &artifact.metadata.entrypoint,
                &artifact.source_files,
                request,
            )
            .map_err(map_artifact_editor_store_error)
    }

    fn artifact_runtime_trace(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<GenUiRuntimeTraceProjection, RuntimeTraceError> {
        let session = self
            .session(session_id)
            .map_err(|_| RuntimeTraceError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(RuntimeTraceError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| RuntimeTraceError::ArtifactUnavailable)?;
        let _guard = self
            .artifact_runtime_trace_lock
            .lock()
            .map_err(|_| RuntimeTraceError::Lock)?;
        self.artifact_runtime_trace_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
            )
            .map_err(map_runtime_trace_store_error)
    }

    fn append_artifact_runtime_trace(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: GenUiRuntimeTraceAppendRequest,
    ) -> Result<GenUiRuntimeTraceProjection, RuntimeTraceError> {
        let session = self
            .session(session_id)
            .map_err(|_| RuntimeTraceError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(RuntimeTraceError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| RuntimeTraceError::ArtifactUnavailable)?;
        if request.source_revision != artifact.metadata.source_revision {
            return Err(RuntimeTraceError::StaleRevision);
        }
        let _guard = self
            .artifact_runtime_trace_lock
            .lock()
            .map_err(|_| RuntimeTraceError::Lock)?;
        self.artifact_runtime_trace_store
            .append(
                session.task_id,
                artifact_id,
                request.source_revision,
                request.events,
            )
            .map_err(map_runtime_trace_store_error)
    }

    fn artifact_debug_capsule(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<GenUiBugCapsule, BugCapsuleRequestError> {
        let session = self
            .session(session_id)
            .map_err(|_| BugCapsuleRequestError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(BugCapsuleRequestError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| BugCapsuleRequestError::ArtifactUnavailable)?;
        let _editor_guard = self
            .artifact_editor_lock
            .lock()
            .map_err(|_| BugCapsuleRequestError::Lock)?;
        let editor = self
            .artifact_editor_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
                &artifact.metadata.entrypoint,
                &artifact.source_files,
            )
            .map_err(|_| BugCapsuleRequestError::Store)?;
        let _trace_guard = self
            .artifact_runtime_trace_lock
            .lock()
            .map_err(|_| BugCapsuleRequestError::Lock)?;
        let runtime = self
            .artifact_runtime_trace_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
            )
            .map_err(|_| BugCapsuleRequestError::Store)?;
        let compiler = self.artifact_draft_compiler.as_deref();
        let environment = GenUiBugCapsuleEnvironment {
            hyper_term_version: env!("CARGO_PKG_VERSION").into(),
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            deno_runtime_version: compiler.map(|compiler| compiler.config.runtime_version.clone()),
            deno_executable_digest: compiler
                .map(|compiler| compiler.config.executable_sha256.clone()),
            compiler_script_digest: compiler
                .map(|compiler| compiler.config.compiler_script_sha256.clone()),
            compiler_wasm_digest: compiler
                .map(|compiler| compiler.config.compiler_wasm_sha256.clone()),
        };
        build_bug_capsule(&artifact, &editor, &runtime, environment)
            .map_err(|_| BugCapsuleRequestError::Store)
    }

    fn artifact_history(
        &self,
        session_id: u16,
        active_artifact_id: ArtifactId,
    ) -> Result<AgentArtifactHistoryResponse, SessionError> {
        let session = self.session(session_id)?;
        let entries = self
            .config
            .daemon
            .genui_artifact_history(session.task_id, active_artifact_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?
            .into_iter()
            .map(|entry| AgentArtifactHistoryEntry {
                event_sequence: entry.event_sequence,
                recorded_at_ms: entry.recorded_at_ms,
                operation_id: entry.operation_id,
                artifact: entry.artifact,
            })
            .collect();
        Ok(AgentArtifactHistoryResponse {
            active_artifact_id,
            entries,
        })
    }

    fn artifact_history_source(
        &self,
        session_id: u16,
        active_artifact_id: ArtifactId,
        revision_id: ArtifactId,
    ) -> Result<AgentArtifactSourceResponse, SessionError> {
        let session = self.session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_genui_artifact_revision(session.task_id, active_artifact_id, revision_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?;
        if artifact.source_files.is_empty() {
            return Err(SessionError::ArtifactUnavailable);
        }
        Ok(AgentArtifactSourceResponse {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            entrypoint: artifact.metadata.entrypoint,
            files: artifact.source_files,
        })
    }

    fn propose_artifact_draft(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        draft: AgentArtifactDraftRequest,
    ) -> Result<AgentArtifactDraftResponse, ArtifactDraftError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactDraftError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(ArtifactDraftError::AcpRequired);
        }
        if self.artifact_draft_compiler.is_none() {
            return Err(ArtifactDraftError::RuntimeUnavailable);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| ArtifactDraftError::ArtifactUnavailable)?;
        let request = validate_artifact_draft(&artifact, draft)?;
        let payload =
            serde_json::to_vec(&request).map_err(|_| ArtifactDraftError::InvalidRequest)?;
        let payload_digest = sha256_bytes(&payload);
        let mut drafts = self
            .artifact_drafts
            .lock()
            .map_err(|_| ArtifactDraftError::Lock)?;
        drafts.retain(|_, record| {
            record.session_id != session_id
                || matches!(
                    record.state,
                    ArtifactDraftState::WaitingApproval | ArtifactDraftState::Compiling
                )
        });
        if drafts.values().any(|record| {
            record.session_id == session_id
                && matches!(
                    record.state,
                    ArtifactDraftState::WaitingApproval | ArtifactDraftState::Compiling
                )
        }) {
            return Err(ArtifactDraftError::Busy);
        }
        let operation = self
            .config
            .daemon
            .propose_operation(
                session.task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest,
                },
                format!(
                    "Publish edited GenUI artifact revision {}",
                    request.source_revision
                ),
                RiskClass::ReadOnly,
                vec![
                    "artifact_build".into(),
                    "deno_runtime".into(),
                    "artifact_publish".into(),
                ],
            )
            .map_err(|_| ArtifactDraftError::Daemon)?;
        let record = ArtifactDraftRecord {
            session_id,
            task_id: session.task_id,
            base_artifact_id: artifact_id,
            base_source_revision: artifact.metadata.source_revision,
            waiting_revision: operation.revision,
            request,
            state: ArtifactDraftState::WaitingApproval,
        };
        let response = artifact_draft_response(operation.operation_id, operation.revision, &record);
        drafts.insert(operation.operation_id, record);
        Ok(response)
    }

    fn artifact_draft_status(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        operation_id: OperationId,
    ) -> Result<AgentArtifactDraftResponse, ArtifactDraftError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactDraftError::SessionUnavailable)?;
        let drafts = self
            .artifact_drafts
            .lock()
            .map_err(|_| ArtifactDraftError::Lock)?;
        let record = drafts
            .get(&operation_id)
            .filter(|record| {
                record.session_id == session_id
                    && record.task_id == session.task_id
                    && record.base_artifact_id == artifact_id
            })
            .ok_or(ArtifactDraftError::ArtifactUnavailable)?;
        let revision = self
            .config
            .daemon
            .operation(operation_id)
            .map(|operation| operation.revision)
            .unwrap_or(record.waiting_revision);
        Ok(artifact_draft_response(operation_id, revision, record))
    }

    fn execute_artifact_draft(&self, operation_id: OperationId, authorized_revision: u64) {
        let record = match self
            .artifact_drafts
            .lock()
            .ok()
            .and_then(|drafts| drafts.get(&operation_id).cloned())
        {
            Some(record) => record,
            None => return,
        };
        let dispatching = match self.config.daemon.begin_operation(
            record.task_id,
            operation_id,
            authorized_revision,
        ) {
            Ok(operation) => operation,
            Err(error) => {
                self.set_artifact_draft_failed(operation_id, &error.to_string());
                return;
            }
        };
        let result = (|| {
            let current = self
                .config
                .daemon
                .read_active_genui_artifact(record.task_id, record.base_artifact_id)
                .map_err(|_| "base artifact is no longer current".to_owned())?;
            if current.metadata.source_revision != record.base_source_revision {
                return Err("base artifact revision is no longer current".to_owned());
            }
            let compiler = self
                .artifact_draft_compiler
                .as_ref()
                .ok_or_else(|| "Rust-supervised Deno compiler is unavailable".to_owned())?;
            let candidate = compiler.compile(record.request.clone())?;
            self.config
                .daemon
                .accept_genui_artifact_from_base(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    record.base_artifact_id,
                    record.base_source_revision,
                    candidate,
                )
                .map_err(|error| error.to_string())
        })();
        match result {
            Ok(artifact) => {
                let completion = OperationCompletion {
                    executor: "hyper-term-artifact-draft".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: format!(
                        "published GenUI artifact revision {}",
                        artifact.source_revision
                    ),
                    result_digest: Some(artifact.content_digest.clone()),
                };
                if let Err(error) = self.config.daemon.complete_operation(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    completion,
                ) {
                    self.set_artifact_draft_failed(operation_id, &error.to_string());
                    return;
                }
                if let Ok(mut drafts) = self.artifact_drafts.lock()
                    && let Some(record) = drafts.get_mut(&operation_id)
                {
                    record.state = ArtifactDraftState::Accepted(artifact);
                }
            }
            Err(message) => {
                let summary = bounded_error(&message);
                let _ = self.config.daemon.complete_operation(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    OperationCompletion {
                        executor: "hyper-term-artifact-draft".into(),
                        succeeded: false,
                        outcome: Some(OperationOutcome::Failed),
                        summary: summary.clone(),
                        result_digest: None,
                    },
                );
                self.set_artifact_draft_failed(operation_id, &summary);
            }
        }
    }

    fn set_artifact_draft_failed(&self, operation_id: OperationId, message: &str) {
        if let Ok(mut drafts) = self.artifact_drafts.lock()
            && let Some(record) = drafts.get_mut(&operation_id)
        {
            record.state = ArtifactDraftState::Failed(bounded_error(message));
        }
    }

    fn preview_workspace_apply(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: AgentWorkspaceApplyRequest,
    ) -> Result<AgentWorkspacePreviewResponse, WorkspaceProposalError> {
        if request.review_digest.is_some() {
            return Err(WorkspaceProposalError::InvalidRequest);
        }
        let artifact_source_revision = request.artifact_source_revision;
        let mappings = normalize_workspace_apply_mappings(request)?;
        if mappings.iter().any(|mapping| !mapping.hunk_ids.is_empty()) {
            return Err(WorkspaceProposalError::InvalidRequest);
        }
        let review = self.prepare_workspace_review(
            session_id,
            artifact_id,
            artifact_source_revision,
            &mappings,
        )?;
        Ok(workspace_preview_response(&review))
    }

    fn prepare_workspace_review(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        artifact_source_revision: u64,
        mappings: &[AgentWorkspaceApplyMapping],
    ) -> Result<PreparedWorkspaceReview, WorkspaceProposalError> {
        let session = self
            .session(session_id)
            .map_err(|_| WorkspaceProposalError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(WorkspaceProposalError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| WorkspaceProposalError::ArtifactUnavailable)?;
        if artifact_source_revision != artifact.metadata.source_revision {
            return Err(WorkspaceProposalError::StaleRevision);
        }
        let mut target_sources = BTreeMap::new();
        let mut plan_requests = Vec::with_capacity(mappings.len());
        for mapping in mappings {
            if target_sources
                .insert(mapping.target_path.clone(), mapping.source_path.clone())
                .is_some()
            {
                return Err(WorkspaceProposalError::InvalidRequest);
            }
            let proposed_content = artifact
                .source_files
                .get(&mapping.source_path)
                .cloned()
                .ok_or(WorkspaceProposalError::InvalidRequest)?;
            plan_requests.push((mapping.target_path.clone(), proposed_content));
        }
        let plan = prepare_workspace_apply_set(&self.config.workspace, plan_requests)
            .map_err(map_workspace_prepare_error)?;
        let source_paths = plan
            .plans
            .iter()
            .map(|plan| {
                target_sources
                    .get(&plan.target_path)
                    .cloned()
                    .ok_or(WorkspaceProposalError::InvalidRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let artifact_source_digests = source_paths
            .iter()
            .map(|source_path| {
                artifact
                    .source_files
                    .get(source_path)
                    .map(|source| sha256_bytes(source.as_bytes()))
                    .ok_or(WorkspaceProposalError::InvalidRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let diffs = plan
            .plans
            .iter()
            .map(|plan| {
                review_workspace_diff(
                    &plan.target_path,
                    plan.base_content(),
                    &plan.proposed_content,
                )
            })
            .collect::<Vec<_>>();
        let review_payload = serde_json::to_vec(&(
            artifact_id,
            artifact_source_revision,
            &source_paths,
            &artifact_source_digests,
            &plan,
            &diffs,
        ))
        .map_err(|_| WorkspaceProposalError::InvalidRequest)?;
        Ok(PreparedWorkspaceReview {
            artifact_source_revision,
            source_paths,
            artifact_source_digests,
            plan,
            diffs,
            review_digest: sha256_bytes(&review_payload),
        })
    }

    fn propose_workspace_apply(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: AgentWorkspaceApplyRequest,
    ) -> Result<AgentWorkspaceApplyResponse, WorkspaceProposalError> {
        if self
            .workspace_recovery_block
            .lock()
            .map_err(|_| WorkspaceProposalError::Lock)?
            .is_some()
        {
            return Err(WorkspaceProposalError::RecoveryRequired);
        }
        let session = self
            .session(session_id)
            .map_err(|_| WorkspaceProposalError::SessionUnavailable)?;
        let artifact_source_revision = request.artifact_source_revision;
        let expected_review_digest = request.review_digest.clone();
        let mappings = normalize_workspace_apply_mappings(request)?;
        let reviewed = self.prepare_workspace_review(
            session_id,
            artifact_id,
            artifact_source_revision,
            &mappings,
        )?;
        let (source_paths, artifact_source_digests, plan, selected_hunk_count) =
            if let Some(expected_review_digest) = expected_review_digest {
                if expected_review_digest != reviewed.review_digest {
                    return Err(WorkspaceProposalError::StaleRevision);
                }
                let mut selections = BTreeMap::new();
                let mut source_paths = Vec::new();
                let mut artifact_source_digests = Vec::new();
                let mut selected_hunk_count = 0_usize;
                for (((source_path, source_digest), reviewed_plan), diff) in reviewed
                    .source_paths
                    .iter()
                    .zip(&reviewed.artifact_source_digests)
                    .zip(&reviewed.plan.plans)
                    .zip(&reviewed.diffs)
                {
                    let mapping = mappings
                        .iter()
                        .find(|mapping| {
                            mapping.source_path == *source_path
                                && mapping.target_path == reviewed_plan.target_path
                        })
                        .ok_or(WorkspaceProposalError::InvalidRequest)?;
                    if mapping.hunk_ids.is_empty() {
                        continue;
                    }
                    let selected_content = select_workspace_hunks(
                        &reviewed_plan.target_path,
                        reviewed_plan.base_content(),
                        &reviewed_plan.proposed_content,
                        &diff.review_digest,
                        &mapping.hunk_ids,
                    )
                    .map_err(|_| WorkspaceProposalError::InvalidRequest)?;
                    selections.insert(reviewed_plan.target_path.clone(), selected_content);
                    source_paths.push(source_path.clone());
                    artifact_source_digests.push(source_digest.clone());
                    selected_hunk_count += mapping.hunk_ids.len();
                }
                let plan = select_workspace_apply_set(&reviewed.plan, selections)
                    .map_err(map_workspace_prepare_error)?;
                (
                    source_paths,
                    artifact_source_digests,
                    plan,
                    selected_hunk_count,
                )
            } else {
                if mappings.iter().any(|mapping| !mapping.hunk_ids.is_empty()) {
                    return Err(WorkspaceProposalError::InvalidRequest);
                }
                let selected_hunk_count = reviewed.diffs.iter().map(|diff| diff.hunks.len()).sum();
                (
                    reviewed.source_paths,
                    reviewed.artifact_source_digests,
                    reviewed.plan,
                    selected_hunk_count,
                )
            };
        let payload = serde_json::to_vec(&(
            artifact_id,
            artifact_source_revision,
            &source_paths,
            &artifact_source_digests,
            selected_hunk_count,
            &plan,
        ))
        .map_err(|_| WorkspaceProposalError::InvalidRequest)?;
        let payload_digest = sha256_bytes(&payload);
        let mut applies = self
            .workspace_applies
            .lock()
            .map_err(|_| WorkspaceProposalError::Lock)?;
        applies.retain(|_, record| {
            record.session_id != session_id
                || matches!(
                    record.state,
                    WorkspaceApplyState::WaitingApproval | WorkspaceApplyState::Applying
                )
        });
        if applies.values().any(|record| {
            record.session_id == session_id
                && matches!(
                    record.state,
                    WorkspaceApplyState::WaitingApproval | WorkspaceApplyState::Applying
                )
        }) {
            return Err(WorkspaceProposalError::Busy);
        }
        let operation = self
            .config
            .daemon
            .propose_operation(
                session.task_id,
                OperationKind::FileEdit,
                OperationAction::Opaque {
                    kind: "hyper_term.workspace.apply".into(),
                    payload_digest,
                },
                format!(
                    "Apply {} selected hunk(s) across {} Artifact source file(s) from r{} to the workspace",
                    selected_hunk_count,
                    plan.plans.len(),
                    artifact_source_revision
                ),
                RiskClass::WorkspaceWrite,
                vec!["workspace_write".into(), "artifact_apply".into()],
            )
            .map_err(|_| WorkspaceProposalError::Daemon)?;
        let record = WorkspaceApplyRecord {
            session_id,
            task_id: session.task_id,
            artifact_id,
            artifact_source_revision,
            source_paths,
            artifact_source_digests,
            selected_hunk_count,
            waiting_revision: operation.revision,
            plan,
            state: WorkspaceApplyState::WaitingApproval,
        };
        let response =
            workspace_apply_response(operation.operation_id, operation.revision, &record);
        applies.insert(operation.operation_id, record);
        Ok(response)
    }

    fn workspace_apply_status(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        operation_id: OperationId,
    ) -> Result<AgentWorkspaceApplyResponse, WorkspaceProposalError> {
        let session = self
            .session(session_id)
            .map_err(|_| WorkspaceProposalError::SessionUnavailable)?;
        let applies = self
            .workspace_applies
            .lock()
            .map_err(|_| WorkspaceProposalError::Lock)?;
        let record = applies
            .get(&operation_id)
            .filter(|record| {
                record.session_id == session_id
                    && record.task_id == session.task_id
                    && record.artifact_id == artifact_id
            })
            .ok_or(WorkspaceProposalError::ArtifactUnavailable)?;
        let revision = self
            .config
            .daemon
            .operation(operation_id)
            .map(|operation| operation.revision)
            .unwrap_or(record.waiting_revision);
        Ok(workspace_apply_response(operation_id, revision, record))
    }

    fn execute_workspace_apply(&self, operation_id: OperationId, authorized_revision: u64) {
        let record = match self
            .workspace_applies
            .lock()
            .ok()
            .and_then(|applies| applies.get(&operation_id).cloned())
        {
            Some(record) => record,
            None => return,
        };
        let dispatching = match self.config.daemon.begin_operation(
            record.task_id,
            operation_id,
            authorized_revision,
        ) {
            Ok(operation) => operation,
            Err(error) => {
                self.set_workspace_apply_failed(operation_id, &error.to_string());
                return;
            }
        };
        let validation: Result<(), String> = (|| {
            let current = self
                .config
                .daemon
                .read_active_genui_artifact(record.task_id, record.artifact_id)
                .map_err(|_| "artifact is no longer current".to_owned())?;
            if current.metadata.source_revision != record.artifact_source_revision {
                return Err("artifact source revision is no longer current".into());
            }
            if record.source_paths.len() != record.plan.plans.len()
                || record.artifact_source_digests.len() != record.plan.plans.len()
            {
                return Err("workspace apply source mapping is inconsistent".into());
            }
            for (source_path, artifact_source_digest) in record
                .source_paths
                .iter()
                .zip(&record.artifact_source_digests)
            {
                let current_source = current
                    .source_files
                    .get(source_path)
                    .ok_or_else(|| "artifact source path is no longer current".to_owned())?;
                if sha256_bytes(current_source.as_bytes()) != *artifact_source_digest {
                    return Err("artifact source digest is no longer current".into());
                }
            }
            Ok(())
        })();
        if let Err(message) = validation {
            let summary = bounded_error(&message);
            let _ = self.config.daemon.complete_operation(
                record.task_id,
                operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "hyper-term-workspace-apply".into(),
                    succeeded: false,
                    outcome: Some(OperationOutcome::Failed),
                    summary: summary.clone(),
                    result_digest: None,
                },
            );
            self.set_workspace_apply_failed(operation_id, &summary);
            return;
        }

        let durable = apply_workspace_set_plan_durable(
            &self.config.workspace,
            &self.config.state_directory,
            WorkspaceTransactionContext {
                task_id: record.task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &record.plan,
        );
        match durable {
            Ok(DurableWorkspaceApplyResult::Committed(receipt)) => {
                self.finish_workspace_transaction(&record, receipt);
            }
            Ok(DurableWorkspaceApplyResult::RolledBack(receipt)) => {
                self.finish_workspace_transaction(&record, receipt);
            }
            Err(error) => {
                let summary = bounded_error(&error.to_string());
                let _ = self.config.daemon.complete_operation(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    OperationCompletion {
                        executor: "hyper-term-workspace-apply".into(),
                        succeeded: false,
                        outcome: Some(OperationOutcome::UnknownExecution),
                        summary: summary.clone(),
                        result_digest: None,
                    },
                );
                self.set_workspace_apply_unknown(operation_id, &summary);
            }
        }
    }

    fn finish_workspace_transaction(
        &self,
        record: &WorkspaceApplyRecord,
        receipt: WorkspaceTransactionReceipt,
    ) {
        let committed = receipt.outcome == WorkspaceTransactionOutcome::Committed;
        let summary = if committed {
            format!(
                "applied {} selected hunk(s) across {} Artifact source file(s) to the workspace",
                record.selected_hunk_count,
                record.plan.plans.len(),
            )
        } else {
            bounded_error(
                receipt
                    .failure_summary
                    .as_deref()
                    .unwrap_or("workspace transaction was rolled back"),
            )
        };
        let completion = OperationCompletion {
            executor: "hyper-term-workspace-apply".into(),
            succeeded: committed,
            outcome: Some(if committed {
                OperationOutcome::Succeeded
            } else {
                OperationOutcome::Failed
            }),
            summary: summary.clone(),
            result_digest: committed.then(|| receipt.result_digest.clone()),
        };
        if let Err(error) = self.config.daemon.complete_operation(
            receipt.task_id,
            receipt.operation_id,
            receipt.operation_revision,
            completion,
        ) {
            self.set_workspace_apply_unknown(receipt.operation_id, &error.to_string());
            return;
        }
        if let Err(error) =
            acknowledge_workspace_transaction(&self.config.state_directory, receipt.transaction_id)
        {
            self.set_workspace_apply_unknown(receipt.operation_id, &error.to_string());
            return;
        }
        if committed {
            if let Ok(mut applies) = self.workspace_applies.lock()
                && let Some(record) = applies.get_mut(&receipt.operation_id)
            {
                record.state = WorkspaceApplyState::Applied;
            }
        } else {
            self.set_workspace_apply_failed(receipt.operation_id, &summary);
        }
    }

    fn set_workspace_apply_failed(&self, operation_id: OperationId, message: &str) {
        if let Ok(mut applies) = self.workspace_applies.lock()
            && let Some(record) = applies.get_mut(&operation_id)
        {
            record.state = WorkspaceApplyState::Failed(bounded_error(message));
        }
    }

    fn set_workspace_apply_unknown(&self, operation_id: OperationId, message: &str) {
        if let Ok(mut applies) = self.workspace_applies.lock()
            && let Some(record) = applies.get_mut(&operation_id)
        {
            record.state = WorkspaceApplyState::UnknownExecution(bounded_error(message));
        }
        if let Ok(mut blocked) = self.workspace_recovery_block.lock() {
            *blocked = Some(bounded_error(message));
        }
    }

    fn editor_lsp_query(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: EditorLspRequest,
    ) -> Result<EditorLspResponse, EditorRequestError> {
        let session = self
            .session(session_id)
            .map_err(|_| EditorRequestError::SessionUnavailable)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(EditorRequestError::AcpRequired);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| EditorRequestError::ArtifactUnavailable)?;
        let service = self
            .editor_lsp
            .as_ref()
            .ok_or(EditorRequestError::RuntimeUnavailable)?;
        service
            .query(session_id, &artifact, request)
            .map_err(|error| match error {
                EditorLspError::StaleRevision => EditorRequestError::StaleRevision,
                EditorLspError::InvalidRequest(_) | EditorLspError::DocumentUnavailable => {
                    EditorRequestError::InvalidRequest
                }
                EditorLspError::InvalidRuntime => EditorRequestError::RuntimeUnavailable,
                _ => EditorRequestError::Driver,
            })
    }

    fn apply_goal_command(
        &self,
        session_id: u16,
        command: &str,
    ) -> Result<AgentTurnResponse, SessionError> {
        let session = self.session(session_id)?;
        {
            let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            if matches!(
                progress.status,
                AgentStatus::Running | AgentStatus::Cancelling | AgentStatus::WaitingApproval
            ) {
                return Err(SessionError::Busy);
            }
        }
        let argument = command
            .trim()
            .strip_prefix("/goal")
            .ok_or(SessionError::InvalidConfig)?
            .trim();
        if argument.eq_ignore_ascii_case("clear") {
            session
                .client
                .clear_thread_goal(&session.thread_id, START_TURN_TIMEOUT)
                .map_err(|error| match error {
                    hyper_term_drivers::AgentClientError::Unsupported(_) => {
                        SessionError::Unsupported
                    }
                    _ => SessionError::Driver,
                })?;
        } else if !argument.is_empty() {
            let (objective, status) = match argument {
                "pause" => (None, Some(AgentGoalStatus::Paused)),
                "resume" => (None, Some(AgentGoalStatus::Active)),
                _ => (Some(argument), Some(AgentGoalStatus::Active)),
            };
            session
                .client
                .set_thread_goal(&session.thread_id, objective, status, START_TURN_TIMEOUT)
                .map_err(|error| match error {
                    hyper_term_drivers::AgentClientError::Unsupported(_) => {
                        SessionError::Unsupported
                    }
                    _ => SessionError::Driver,
                })?;
        }
        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Ready,
        })
    }

    fn submit_turn(
        &self,
        session_id: u16,
        prompt: String,
    ) -> Result<AgentTurnResponse, SessionError> {
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(SessionError::PromptTooLarge);
        }
        let session = self.session(session_id)?;
        {
            let mut progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            if matches!(
                progress.status,
                AgentStatus::Running | AgentStatus::Cancelling | AgentStatus::WaitingApproval
            ) {
                return Err(SessionError::Busy);
            }
            progress.status = AgentStatus::Running;
            progress.turn_id = None;
            progress.error = None;
        }
        self.config
            .daemon
            .append_message(
                session.task_id,
                BlockId::new(),
                MessageRole::User,
                None,
                prompt.clone(),
            )
            .map_err(|_| SessionError::Daemon)?;
        let daemon = self.config.daemon.clone();
        let worker_session = Arc::clone(&session);
        std::thread::Builder::new()
            .name(format!("hyper-term-agent-{session_id}"))
            .spawn(move || run_turn(worker_session, daemon, prompt))
            .map_err(|_| {
                set_progress_failed(&session, "Agent turn worker could not start");
                SessionError::Thread
            })?;
        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Running,
        })
    }

    fn cancel_turn(&self, session_id: u16) -> Result<AgentTurnResponse, SessionError> {
        let session = self.session(session_id)?;
        let (turn_id, waiting_approval) = {
            let mut progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            match progress.status {
                AgentStatus::Cancelling => {
                    return Ok(AgentTurnResponse {
                        session_id,
                        status: AgentStatus::Cancelling,
                    });
                }
                AgentStatus::Running | AgentStatus::WaitingApproval => {}
                _ => return Err(SessionError::NoActiveTurn),
            }
            let waiting_approval = progress.status == AgentStatus::WaitingApproval;
            progress.status = AgentStatus::Cancelling;
            progress.error = None;
            (progress.turn_id.clone(), waiting_approval)
        };

        let pending = session
            .pending_effect
            .lock()
            .map_err(|_| SessionError::Lock)?
            .take();
        let projection = if let Some(effect) = pending {
            let decided = self
                .config
                .daemon
                .decide_permission(
                    session.task_id,
                    effect.operation_id,
                    effect.operation_revision,
                    PermissionDecision::Cancelled,
                )
                .map_err(|_| SessionError::StalePermission)?;
            if let Some(host_request) = &effect.host_request {
                session
                    .client
                    .resolve_host_request(
                        &host_request.request_id,
                        AgentHostResponse::Error {
                            code: -32800,
                            message: "Agent turn cancelled by user".into(),
                        },
                    )
                    .map_err(|_| SessionError::Driver)?;
            } else {
                session
                    .client
                    .resolve_effect(
                        &effect.request_id,
                        AgentEffectAuthorization {
                            operation_id: effect.operation_id,
                            operation_revision: decided.revision,
                            proposal_sha256: effect.payload_sha256,
                            decision: PermissionDecision::Cancelled,
                        },
                    )
                    .map_err(|_| SessionError::Driver)?;
            }
            Some(effect.projection)
        } else {
            None
        };

        if let Some(turn_id) = turn_id.as_deref()
            && session
                .client
                .cancel_turn(&session.thread_id, turn_id)
                .is_err()
        {
            set_progress_failed(&session, "Agent turn cancellation could not be delivered");
            return Err(SessionError::Driver);
        }

        if waiting_approval && projection.is_none() {
            return Err(SessionError::NoPendingEffect);
        }
        if let Some(projection) = projection {
            let daemon = self.config.daemon.clone();
            let worker_session = Arc::clone(&session);
            std::thread::Builder::new()
                .name(format!("hyper-term-agent-{session_id}-cancel"))
                .spawn(move || continue_turn(worker_session, daemon, projection))
                .map_err(|_| {
                    set_progress_failed(&session, "Agent cancellation worker could not start");
                    SessionError::Thread
                })?;
        }

        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Cancelling,
        })
    }

    fn set_session_config(
        &self,
        session_id: u16,
        request: AgentConfigRequest,
    ) -> Result<AgentCapabilitiesResponse, SessionError> {
        let session = self.session(session_id)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(SessionError::Unsupported);
        }
        {
            let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            if matches!(
                progress.status,
                AgentStatus::Running | AgentStatus::Cancelling | AgentStatus::WaitingApproval
            ) {
                return Err(SessionError::Busy);
            }
        }
        let capabilities = session
            .client
            .set_session_config_option(
                &session.thread_id,
                &request.config_id,
                request.value,
                START_TURN_TIMEOUT,
            )
            .map_err(|error| match error {
                hyper_term_drivers::AgentClientError::Acp(
                    hyper_term_drivers::AcpAdapterError::InvalidMessage(_),
                ) => SessionError::InvalidConfig,
                hyper_term_drivers::AgentClientError::Unsupported(_) => SessionError::Unsupported,
                _ => SessionError::Driver,
            })?;
        Ok(AgentCapabilitiesResponse {
            session_id,
            capabilities,
        })
    }

    fn tier2_results(&self, session_id: u16) -> Result<AgentTier2ResultsResponse, SessionError> {
        let session = self.session(session_id)?;
        let acceptances = self
            .config
            .daemon
            .isolated_acceptance_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .into_iter()
            .map(|review| (review.source_operation_id, tier2_review_response(review)))
            .collect::<HashMap<_, _>>();
        let results = self
            .config
            .daemon
            .isolated_result_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .into_iter()
            .map(|review| AgentTier2ResultResponse {
                source_operation_id: review.operation_id,
                source_revision: review.receipt.source_revision,
                finished_at_ms: review.receipt.finished_at_ms,
                termination: review.receipt.termination,
                exit_code: review.receipt.exit_code,
                changed_bytes: review.receipt.changes.changed_bytes,
                inventory_sha256: review.receipt.changes.inventory_sha256,
                changed_files: review.receipt.changes.changed_files,
                acceptance: acceptances.get(&review.operation_id).cloned(),
            })
            .collect();
        Ok(AgentTier2ResultsResponse { results })
    }

    fn preview_tier2_result(
        &self,
        session_id: u16,
        source_operation_id: OperationId,
    ) -> Result<AgentTier2PreviewResponse, SessionError> {
        let session = self.session(session_id)?;
        let preview = self
            .config
            .daemon
            .preview_isolated_result_acceptance(session.task_id, source_operation_id)
            .map_err(|error| match error {
                DaemonError::IsolatedResultMissing(_) => SessionError::NotFound,
                _ => SessionError::Daemon,
            })?;
        Ok(tier2_preview_response(preview))
    }

    fn propose_tier2_review(
        &self,
        session_id: u16,
        source_operation_id: OperationId,
    ) -> Result<AgentTier2ReviewResponse, SessionError> {
        let session = self.session(session_id)?;
        if !self
            .config
            .daemon
            .isolated_result_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .iter()
            .any(|result| result.operation_id == source_operation_id)
        {
            return Err(SessionError::NotFound);
        }
        let review = self
            .config
            .daemon
            .propose_isolated_result_acceptance(session.task_id, source_operation_id)
            .map_err(|error| match error {
                DaemonError::IsolatedAcceptanceAlreadyExists(_) => SessionError::Busy,
                DaemonError::IsolatedResultMissing(_) => SessionError::NotFound,
                _ => SessionError::Daemon,
            })?;
        Ok(tier2_review_response(review))
    }

    fn discard_tier2_result(
        &self,
        session_id: u16,
        source_operation_id: OperationId,
    ) -> Result<(), SessionError> {
        let session = self.session(session_id)?;
        if !self
            .config
            .daemon
            .isolated_result_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .iter()
            .any(|result| result.operation_id == source_operation_id)
        {
            return Err(SessionError::NotFound);
        }
        self.config
            .daemon
            .discard_isolated_result(source_operation_id)
            .map_err(|error| match error {
                DaemonError::IsolatedResultHasPendingAcceptance(_) => SessionError::Busy,
                DaemonError::IsolatedResultMissing(_) => SessionError::NotFound,
                _ => SessionError::Daemon,
            })
    }

    fn decide_effect(
        &self,
        session_id: u16,
        request: AgentPermissionRequest,
    ) -> Result<AgentTurnResponse, SessionError> {
        if !matches!(
            request.decision,
            PermissionDecision::AllowOnce
                | PermissionDecision::RejectOnce
                | PermissionDecision::Cancelled
        ) {
            return Err(SessionError::UnsafeApproval);
        }
        let session = self.session(session_id)?;
        let mut pending = session
            .pending_effect
            .lock()
            .map_err(|_| SessionError::Lock)?;
        let effect = pending
            .as_ref()
            .filter(|effect| {
                effect.operation_id == request.operation_id
                    && effect.operation_revision == request.expected_revision
            })
            .cloned();
        if effect.is_none() {
            drop(pending);
            let operation = self
                .config
                .daemon
                .operation(request.operation_id)
                .map_err(|_| SessionError::NoPendingEffect)?;
            if operation.task_id != session.task_id
                || operation.revision != request.expected_revision
                || operation.state != hyper_term_protocol::OperationState::WaitingHuman
            {
                return Err(SessionError::StalePermission);
            }
            let draft = self
                .artifact_drafts
                .lock()
                .map_err(|_| SessionError::Lock)?
                .get(&request.operation_id)
                .cloned();
            let workspace_apply = self
                .workspace_applies
                .lock()
                .map_err(|_| SessionError::Lock)?
                .get(&request.operation_id)
                .cloned();
            let tier2_review = match self
                .config
                .daemon
                .isolated_acceptance_review(request.operation_id)
            {
                Ok(review) => Some(review),
                Err(DaemonError::IsolatedAcceptanceMissing(_)) => None,
                Err(_) => return Err(SessionError::Daemon),
            };
            if request.decision == PermissionDecision::AllowOnce
                && workspace_apply.is_none()
                && tier2_review.is_none()
                && !allowable_brokered_mcp_operation(&operation)
            {
                return Err(SessionError::UnsafeApproval);
            }
            if let Some(draft) = draft {
                if draft.session_id != session_id
                    || draft.task_id != session.task_id
                    || draft.waiting_revision != request.expected_revision
                    || !matches!(draft.state, ArtifactDraftState::WaitingApproval)
                {
                    return Err(SessionError::StalePermission);
                }
                let decided = self
                    .config
                    .daemon
                    .decide_permission(
                        session.task_id,
                        request.operation_id,
                        request.expected_revision,
                        request.decision,
                    )
                    .map_err(|_| SessionError::StalePermission)?;
                {
                    let mut drafts = self
                        .artifact_drafts
                        .lock()
                        .map_err(|_| SessionError::Lock)?;
                    let record = drafts
                        .get_mut(&request.operation_id)
                        .ok_or(SessionError::StalePermission)?;
                    record.state = if request.decision == PermissionDecision::AllowOnce {
                        ArtifactDraftState::Compiling
                    } else {
                        ArtifactDraftState::Rejected
                    };
                }
                if request.decision == PermissionDecision::AllowOnce {
                    let runtime = self.clone();
                    std::thread::Builder::new()
                        .name(format!("hyper-term-artifact-draft-{session_id}"))
                        .spawn(move || {
                            runtime.execute_artifact_draft(request.operation_id, decided.revision)
                        })
                        .map_err(|_| {
                            self.set_artifact_draft_failed(
                                request.operation_id,
                                "Artifact draft worker could not start",
                            );
                            SessionError::Thread
                        })?;
                }
                let status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                return Ok(AgentTurnResponse { session_id, status });
            }
            if let Some(workspace_apply) = workspace_apply {
                if workspace_apply.session_id != session_id
                    || workspace_apply.task_id != session.task_id
                    || workspace_apply.waiting_revision != request.expected_revision
                    || !matches!(workspace_apply.state, WorkspaceApplyState::WaitingApproval)
                    || operation.kind != OperationKind::FileEdit
                    || operation.risk != RiskClass::WorkspaceWrite
                    || !matches!(
                        &operation.action,
                        OperationAction::Opaque { kind, .. }
                            if kind == "hyper_term.workspace.apply"
                    )
                {
                    return Err(SessionError::StalePermission);
                }
                let decided = self
                    .config
                    .daemon
                    .decide_permission(
                        session.task_id,
                        request.operation_id,
                        request.expected_revision,
                        request.decision,
                    )
                    .map_err(|_| SessionError::StalePermission)?;
                {
                    let mut applies = self
                        .workspace_applies
                        .lock()
                        .map_err(|_| SessionError::Lock)?;
                    let record = applies
                        .get_mut(&request.operation_id)
                        .ok_or(SessionError::StalePermission)?;
                    record.state = if request.decision == PermissionDecision::AllowOnce {
                        WorkspaceApplyState::Applying
                    } else {
                        WorkspaceApplyState::Rejected
                    };
                }
                if request.decision == PermissionDecision::AllowOnce {
                    let runtime = self.clone();
                    std::thread::Builder::new()
                        .name(format!("hyper-term-workspace-apply-{session_id}"))
                        .spawn(move || {
                            runtime.execute_workspace_apply(request.operation_id, decided.revision)
                        })
                        .map_err(|_| {
                            self.set_workspace_apply_failed(
                                request.operation_id,
                                "Workspace apply worker could not start",
                            );
                            SessionError::Thread
                        })?;
                }
                let status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                return Ok(AgentTurnResponse { session_id, status });
            }
            if let Some(review) = tier2_review {
                if review.operation.task_id != session.task_id
                    || review.operation.revision != request.expected_revision
                    || review.operation.state != OperationState::WaitingHuman
                    || review.operation.kind != OperationKind::FileEdit
                    || review.operation.risk != RiskClass::WorkspaceWrite
                    || !matches!(
                        &review.operation.action,
                        OperationAction::Opaque { kind, .. }
                            if kind == "hyper_term.tier2.accept"
                    )
                {
                    return Err(SessionError::StalePermission);
                }
                let decided = self
                    .config
                    .daemon
                    .decide_isolated_acceptance_permission(
                        session.task_id,
                        request.operation_id,
                        request.expected_revision,
                        request.decision,
                    )
                    .map_err(|_| SessionError::StalePermission)?;
                if request.decision == PermissionDecision::AllowOnce {
                    self.config
                        .daemon
                        .accept_isolated_result(
                            session.task_id,
                            request.operation_id,
                            decided.revision,
                        )
                        .map_err(|_| SessionError::Daemon)?;
                }
                let status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                return Ok(AgentTurnResponse { session_id, status });
            }
            self.config
                .daemon
                .decide_permission(
                    session.task_id,
                    request.operation_id,
                    request.expected_revision,
                    request.decision,
                )
                .map_err(|_| SessionError::StalePermission)?;
            let status = session
                .progress
                .lock()
                .map_err(|_| SessionError::Lock)?
                .status;
            return Ok(AgentTurnResponse { session_id, status });
        }
        let effect = effect.expect("checked pending effect");
        if let Some(host_request) = effect.host_request.clone() {
            let AgentHostOperation::TerminalCreate { .. } = host_request.operation else {
                return Err(SessionError::Driver);
            };
            let runner = if request.decision == PermissionDecision::AllowOnce {
                Some(
                    self.config
                        .tier2_runner
                        .clone()
                        .ok_or(SessionError::UnsafeApproval)?,
                )
            } else {
                None
            };
            let decided = self
                .config
                .daemon
                .decide_permission(
                    session.task_id,
                    effect.operation_id,
                    effect.operation_revision,
                    request.decision,
                )
                .map_err(|_| SessionError::StalePermission)?;
            pending.take();
            drop(pending);
            if let Ok(mut progress) = session.progress.lock() {
                progress.status = AgentStatus::Running;
                progress.error = None;
            } else {
                let _ = session.client.close();
                return Err(SessionError::Lock);
            }
            let projection = effect.projection;
            let daemon = self.config.daemon.clone();
            let worker_session = Arc::clone(&session);
            if let Some(runner) = runner {
                std::thread::Builder::new()
                    .name(format!("hyper-term-agent-{session_id}-terminal"))
                    .spawn(move || {
                        execute_agent_terminal_create(
                            worker_session,
                            daemon,
                            runner,
                            host_request,
                            effect.operation_id,
                            decided.revision,
                            projection,
                        )
                    })
                    .map_err(|_| {
                        set_progress_failed(&session, "ACP terminal worker could not start");
                        SessionError::Thread
                    })?;
            } else {
                if session
                    .client
                    .resolve_host_request(
                        &host_request.request_id,
                        AgentHostResponse::Error {
                            code: -32000,
                            message: "ACP terminal request was not approved".into(),
                        },
                    )
                    .is_err()
                {
                    set_progress_failed(&session, "ACP terminal decision could not be returned");
                    let _ = session.client.close();
                    return Err(SessionError::Driver);
                }
                std::thread::Builder::new()
                    .name(format!("hyper-term-agent-{session_id}-resume"))
                    .spawn(move || continue_turn(worker_session, daemon, projection))
                    .map_err(|_| {
                        set_progress_failed(&session, "Agent turn resume worker could not start");
                        SessionError::Thread
                    })?;
            }
            return Ok(AgentTurnResponse {
                session_id,
                status: AgentStatus::Running,
            });
        }
        if request.decision == PermissionDecision::AllowOnce {
            return Err(SessionError::UnsafeApproval);
        }
        let decided = self
            .config
            .daemon
            .decide_permission(
                session.task_id,
                effect.operation_id,
                effect.operation_revision,
                request.decision,
            )
            .map_err(|_| SessionError::StalePermission)?;
        if session
            .client
            .resolve_effect(
                &effect.request_id,
                AgentEffectAuthorization {
                    operation_id: effect.operation_id,
                    operation_revision: decided.revision,
                    proposal_sha256: effect.payload_sha256,
                    decision: request.decision,
                },
            )
            .is_err()
        {
            set_progress_failed(&session, "Agent effect decision could not be returned");
            let _ = session.client.close();
            return Err(SessionError::Driver);
        }
        pending.take();
        drop(pending);
        if let Ok(mut progress) = session.progress.lock() {
            progress.status = AgentStatus::Running;
            progress.error = None;
        } else {
            let _ = session.client.close();
            return Err(SessionError::Lock);
        }
        let daemon = self.config.daemon.clone();
        let projection = effect.projection;
        let worker_session = Arc::clone(&session);
        std::thread::Builder::new()
            .name(format!("hyper-term-agent-{session_id}-resume"))
            .spawn(move || continue_turn(worker_session, daemon, projection))
            .map_err(|_| {
                set_progress_failed(&session, "Agent turn resume worker could not start");
                SessionError::Thread
            })?;
        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Running,
        })
    }

    fn session(&self, session_id: u16) -> Result<Arc<AgentSession>, SessionError> {
        self.sessions
            .lock()
            .map_err(|_| SessionError::Lock)?
            .get(&session_id)
            .cloned()
            .ok_or(SessionError::NotFound)
    }

    fn close_session(
        &self,
        session_id: u16,
        forget_history: bool,
    ) -> Result<Option<TaskId>, SessionError> {
        if forget_history {
            self.session_bindings
                .forget(session_id)
                .map_err(|_| SessionError::Daemon)?;
        }
        self.close_artifact_drafts(session_id);
        self.close_workspace_applies(session_id);
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id));
        if let Some(session) = session {
            let task_id = session.task_id;
            let _ = session.client.close();
            self.cleanup_brokered_mcp_runtime(session.task_id);
            let _ = std::fs::remove_dir_all(&session.runtime_root);
            if let Some(editor_lsp) = &self.editor_lsp {
                editor_lsp.close_session(session_id);
            }
            return Ok(Some(task_id));
        }
        if let Some(editor_lsp) = &self.editor_lsp {
            editor_lsp.close_session(session_id);
        }
        Ok(None)
    }

    fn close_all(&self) {
        let session_ids = self
            .sessions
            .lock()
            .map(|sessions| sessions.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for session_id in session_ids {
            let _ = self.close_session(session_id, false);
        }
        if let Some(editor_lsp) = &self.editor_lsp {
            editor_lsp.close_all();
        }
        if let Some(compiler) = &self.artifact_draft_compiler {
            compiler.close();
        }
    }

    fn brokered_mcp_root(&self, task_id: TaskId) -> PathBuf {
        self.config
            .state_directory
            .join("brokered-mcp")
            .join(task_id.to_string())
    }

    fn cleanup_brokered_mcp_runtime(&self, task_id: TaskId) {
        let _ = self.config.daemon.unregister_brokered_mcp_runtime(task_id);
        let _ = std::fs::remove_dir_all(self.brokered_mcp_root(task_id));
    }

    fn close_artifact_drafts(&self, session_id: u16) {
        let waiting = self
            .artifact_drafts
            .lock()
            .map(|drafts| {
                drafts
                    .iter()
                    .filter_map(|(operation_id, record)| {
                        (record.session_id == session_id
                            && matches!(record.state, ArtifactDraftState::WaitingApproval))
                        .then_some((*operation_id, record.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (operation_id, record) in waiting {
            let _ = self.config.daemon.decide_permission(
                record.task_id,
                operation_id,
                record.waiting_revision,
                PermissionDecision::Cancelled,
            );
        }
        if let Ok(mut drafts) = self.artifact_drafts.lock() {
            drafts.retain(|_, record| record.session_id != session_id);
        }
    }

    fn close_workspace_applies(&self, session_id: u16) {
        let waiting = self
            .workspace_applies
            .lock()
            .map(|applies| {
                applies
                    .iter()
                    .filter_map(|(operation_id, record)| {
                        (record.session_id == session_id
                            && matches!(record.state, WorkspaceApplyState::WaitingApproval))
                        .then_some((*operation_id, record.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (operation_id, record) in waiting {
            let _ = self.config.daemon.decide_permission(
                record.task_id,
                operation_id,
                record.waiting_revision,
                PermissionDecision::Cancelled,
            );
        }
        if let Ok(mut applies) = self.workspace_applies.lock() {
            applies.retain(|_, record| record.session_id != session_id);
        }
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
