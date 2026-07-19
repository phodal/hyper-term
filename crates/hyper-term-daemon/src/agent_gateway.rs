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
use hyper_term_drivers::{
    AcpAgentClient, AcpAgentConfig, AcpMcpServerConfig, AgentContainmentConfig, AgentDriverEvent,
    AgentEffectAuthorization, AgentEffectKind, AgentSessionCapabilities, AgentSessionConfigValue,
    CodexAppServerClient, CodexAppServerConfig, CodexMcpServerConfig, DenoGenUiCompiler,
    DenoGenUiConfig, DriverState, ExternalRequestId, GenUiCompileRequest, StructuredAgentClient,
    StructuredAgentProtocol, sha256_file, stage_codex_auth_file,
};
use hyper_term_protocol::{
    AcceptedGenUiArtifact, ArtifactId, BlockDocument, BlockId, BlockPatch, GenUiArtifactCandidate,
    GenUiBugCapsule, GenUiBugCapsuleEnvironment, GenUiRuntimeTraceAppendRequest,
    GenUiRuntimeTraceProjection, MessageRole, OperationAction, OperationCompletion, OperationId,
    OperationKind, OperationOutcome, OperationState, PermissionDecision, RiskClass, TaskId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::DaemonState;
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
    MAX_WORKSPACE_HUNKS_PER_FILE, WorkspaceDiffHunk, WorkspaceDiffReview, review_workspace_diff,
    select_workspace_hunks,
};
use crate::workspace_snapshot::{create_private_runtime_root, create_workspace_snapshot};

const MIN_TOKEN_BYTES: usize = 32;
const MAX_AGENT_SESSIONS: usize = 8;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
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
const MAX_ARTIFACT_DRAFT_FILES: usize = 100;
const MAX_ARTIFACT_DRAFT_SOURCE_BYTES: usize = 1024 * 1024;
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
    pub codex_executable: Option<PathBuf>,
    pub codex_auth_file: Option<PathBuf>,
    pub acp_providers: Vec<AcpAgentProviderConfig>,
    pub mcp_executable: Option<PathBuf>,
    pub genui_runtime: Option<AgentGenUiRuntimeConfig>,
    pub workbench_assets: Option<PathBuf>,
    pub debug_capsule: Option<GenUiBugCapsule>,
    pub control_socket: PathBuf,
}

#[derive(Clone, Debug)]
pub struct AcpAgentProviderConfig {
    pub provider_id: String,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<String, OsString>,
    pub implementation_version: String,
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
    #[error("agent gateway GenUI runtime is invalid: {0}")]
    InvalidGenUiRuntime(String),
    #[error("agent gateway Workbench assets are invalid: {0}")]
    InvalidWorkbenchAssets(PathBuf),
    #[error("agent gateway workspace recovery failed: {0}")]
    WorkspaceRecovery(String),
    #[error("agent gateway ACP provider is invalid: {0}")]
    InvalidAcpProvider(String),
    #[error("agent gateway I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent gateway task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
struct AgentGatewayRuntime {
    config: Arc<AgentGatewayConfig>,
    sessions: Arc<Mutex<HashMap<u16, Arc<AgentSession>>>>,
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
    runtime_root: PathBuf,
    progress: Mutex<AgentProgress>,
    pending_effect: Mutex<Option<PendingAgentEffect>>,
    _managed_proxy: Option<ManagedConnectProxy>,
}

struct LaunchedAgentProvider {
    client: Arc<dyn StructuredAgentClient>,
    managed_proxy: Option<ManagedConnectProxy>,
}

#[derive(Clone)]
struct AgentTurnProjection {
    turn_id: String,
    agent_block_id: BlockId,
    agent_message_phase: u32,
    plan_block_id: BlockId,
    agent_message_bytes: usize,
    agent_message_interrupted: bool,
    plan_bytes: usize,
}

#[derive(Clone)]
struct PendingAgentEffect {
    request_id: ExternalRequestId,
    payload_sha256: String,
    operation_id: OperationId,
    operation_revision: u64,
    projection: AgentTurnProjection,
}

#[derive(Clone)]
struct BrokeredMcpLaunch {
    executable: PathBuf,
    executable_sha256: String,
    arguments: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentStatus {
    Ready,
    Running,
    Completed,
    WaitingApproval,
    Failed,
}

struct AgentProgress {
    status: AgentStatus,
    turn_id: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct AgentSessionQuery {
    token: Option<String>,
    session_id: Option<u16>,
    provider: Option<String>,
}

#[derive(Serialize)]
struct AgentSessionResponse {
    session_id: u16,
    provider: String,
    protocol: String,
    status: &'static str,
    task_id: TaskId,
    thread_id: String,
}

#[derive(Serialize)]
struct AgentSnapshotResponse {
    session_id: u16,
    status: AgentStatus,
    turn_id: Option<String>,
    error: Option<String>,
    capabilities: AgentSessionCapabilities,
    document: BlockDocument,
}

#[derive(Deserialize)]
struct AgentConfigRequest {
    config_id: String,
    value: AgentSessionConfigValue,
}

#[derive(Serialize)]
struct AgentCapabilitiesResponse {
    session_id: u16,
    capabilities: AgentSessionCapabilities,
}

#[derive(Serialize)]
struct AgentTurnResponse {
    session_id: u16,
    status: AgentStatus,
}

#[derive(Serialize)]
struct AgentArtifactSourceResponse {
    artifact_id: ArtifactId,
    source_revision: u64,
    entrypoint: String,
    files: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct AgentArtifactHistoryResponse {
    active_artifact_id: ArtifactId,
    entries: Vec<AgentArtifactHistoryEntry>,
}

#[derive(Serialize)]
struct AgentArtifactHistoryEntry {
    event_sequence: u64,
    recorded_at_ms: u64,
    operation_id: Option<OperationId>,
    artifact: AcceptedGenUiArtifact,
}

#[derive(Deserialize)]
struct AgentArtifactDraftRequest {
    base_source_revision: u64,
    entrypoint: String,
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct AgentArtifactDraftStatusQuery {
    token: Option<String>,
    session_id: Option<u16>,
    operation_id: Option<OperationId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactDraftStatus {
    WaitingApproval,
    Compiling,
    Accepted,
    Rejected,
    Failed,
}

#[derive(Serialize)]
struct AgentArtifactDraftResponse {
    operation_id: OperationId,
    operation_revision: u64,
    status: ArtifactDraftStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<AcceptedGenUiArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Deserialize)]
struct AgentWorkspaceApplyRequest {
    artifact_source_revision: u64,
    #[serde(default)]
    review_digest: Option<String>,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    target_path: Option<String>,
    #[serde(default)]
    mappings: Vec<AgentWorkspaceApplyMapping>,
}

#[derive(Deserialize)]
struct AgentWorkspaceApplyMapping {
    source_path: String,
    target_path: String,
    #[serde(default)]
    hunk_ids: Vec<String>,
}

#[derive(Serialize)]
struct AgentWorkspacePreviewResponse {
    artifact_source_revision: u64,
    review_digest: String,
    changes: Vec<AgentWorkspacePreviewChangeResponse>,
}

#[derive(Serialize)]
struct AgentWorkspacePreviewChangeResponse {
    source_path: String,
    target_path: String,
    base_digest: Option<String>,
    artifact_digest: String,
    before: String,
    artifact_after: String,
    hunks: Vec<WorkspaceDiffHunk>,
}

#[derive(Deserialize)]
struct AgentWorkspaceApplyStatusQuery {
    token: Option<String>,
    session_id: Option<u16>,
    operation_id: Option<OperationId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkspaceApplyStatus {
    WaitingApproval,
    Applying,
    Applied,
    Rejected,
    Failed,
    UnknownExecution,
}

#[derive(Serialize)]
struct AgentWorkspaceApplyResponse {
    operation_id: OperationId,
    operation_revision: u64,
    status: WorkspaceApplyStatus,
    artifact_source_revision: u64,
    source_path: String,
    target_path: String,
    base_digest: Option<String>,
    proposed_digest: String,
    before: String,
    after: String,
    transaction_digest: String,
    changes: Vec<AgentWorkspaceApplyChangeResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct AgentWorkspaceApplyChangeResponse {
    source_path: String,
    target_path: String,
    base_digest: Option<String>,
    proposed_digest: String,
    before: String,
    after: String,
}

#[derive(Deserialize)]
struct AgentPermissionRequest {
    operation_id: OperationId,
    expected_revision: u64,
    decision: PermissionDecision,
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

    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let runtime = AgentGatewayRuntime {
        config: Arc::new(config),
        sessions: Arc::new(Mutex::new(HashMap::new())),
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
            "/agent/session",
            get(snapshot_session)
                .post(start_session)
                .delete(close_session),
        )
        .route("/agent/session/turn", post(start_turn))
        .route("/agent/session/stream", get(stream_session))
        .route("/agent/session/config", post(set_session_config))
        .route("/agent/session/permission", post(decide_permission))
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

async fn workbench_index(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    if runtime.session(session_id).is_err() {
        return status_response(StatusCode::NOT_FOUND, "Agent session does not exist");
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
    runtime.close_session(session_id);
    secure_response(
        StatusCode::NO_CONTENT,
        "text/plain; charset=utf-8",
        Body::empty(),
    )
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
    capabilities: AgentSessionCapabilities,
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
    match runtime.submit_turn(session_id, prompt) {
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
        Err(_) => status_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Agent turn could not be started",
        ),
    }
}

async fn decide_permission(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
    body: Bytes,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let request = match serde_json::from_slice::<AgentPermissionRequest>(&body) {
        Ok(request) => request,
        Err(_) => {
            return status_response(StatusCode::BAD_REQUEST, "Permission decision is invalid");
        }
    };
    let result =
        tokio::task::spawn_blocking(move || runtime.decide_effect(session_id, request)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::ACCEPTED, &response),
        Ok(Err(SessionError::NotFound)) => {
            status_response(StatusCode::NOT_FOUND, "Agent session does not exist")
        }
        Ok(Err(SessionError::UnsafeApproval)) => status_response(
            StatusCode::FORBIDDEN,
            "Allow is unavailable until the Rust sandbox can enforce the exact effect",
        ),
        Ok(Err(SessionError::NoPendingEffect | SessionError::StalePermission)) => status_response(
            StatusCode::CONFLICT,
            "Permission decision no longer matches the pending effect",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Permission decision could not be delivered safely",
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
        "claude-acp" => &[".claude", ".claude.json", ".config/claude"],
        "copilot-acp" => &[".config/github-copilot", ".config/gh"],
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

#[cfg(unix)]
fn stage_acp_codex_preferences(home: &Path, codex_home: &Path) -> Result<(), std::io::Error> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::symlink;

    if !home.is_absolute() || !codex_home.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "ACP Codex homes must be absolute",
        ));
    }
    let source_root = home.join(".codex");
    let Ok(canonical_root) = source_root.canonicalize() else {
        return Ok(());
    };
    for relative in ["config.toml", "AGENTS.md"] {
        let source = source_root.join(relative);
        let Ok(metadata) = std::fs::symlink_metadata(&source) else {
            continue;
        };
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ACP Codex preference source is not a regular file or directory",
            ));
        }
        let canonical = source.canonicalize()?;
        if !canonical.starts_with(&canonical_root) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ACP Codex preference source escaped its root",
            ));
        }
        let target = codex_home.join(relative);
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "ACP Codex preference target already exists",
            ));
        }
        symlink(canonical, target)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn stage_acp_codex_preferences(_home: &Path, _codex_home: &Path) -> Result<(), std::io::Error> {
    Ok(())
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
        let task_id = self
            .config
            .daemon
            .create_task(format!("{provider_id} Agent session {session_id}"))
            .map_err(|_| StartError::Driver)?;
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
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(error);
            }
        };
        let protocol = launched.client.protocol();
        let thread_id = match launched.client.initialize_session(INITIALIZE_TIMEOUT) {
            Ok(thread_id) => thread_id,
            Err(_) => {
                let _ = launched.client.close();
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(StartError::Driver);
            }
        };
        let session = Arc::new(AgentSession {
            client: launched.client,
            provider_id: provider_id.to_owned(),
            protocol,
            task_id,
            thread_id,
            runtime_root: session_root,
            progress: Mutex::new(AgentProgress {
                status: AgentStatus::Ready,
                turn_id: None,
                error: None,
            }),
            pending_effect: Mutex::new(None),
            _managed_proxy: launched.managed_proxy,
        });
        let response = ready_response(session_id, &session);
        sessions.insert(session_id, session);
        Ok(response)
    }

    fn launch_provider(
        &self,
        provider_id: &str,
        session_root: &std::path::Path,
        mcp: Option<BrokeredMcpLaunch>,
    ) -> Result<LaunchedAgentProvider, StartError> {
        if provider_id == "codex" {
            let executable = self
                .config
                .codex_executable
                .as_ref()
                .ok_or(StartError::Unavailable)?
                .canonicalize()
                .map_err(|_| StartError::Unavailable)?;
            let executable_sha256 = sha256_file(&executable).map_err(|_| StartError::Driver)?;
            let managed_proxy = ManagedConnectProxy::start(
                CODEX_NETWORK_ALLOWED_HOSTS
                    .iter()
                    .map(|host| (*host).to_owned()),
            )
            .map_err(|_| StartError::Driver)?;
            let endpoint = managed_proxy.endpoint();
            let allowed_unix_sockets = mcp
                .as_ref()
                .map(|_| vec![self.config.control_socket.clone()])
                .unwrap_or_default();
            let mut read_paths = Vec::new();
            if let Some(runtime) = &self.config.genui_runtime {
                read_paths.extend([
                    runtime.deno_executable.clone(),
                    runtime.compiler_script.clone(),
                    runtime.compiler_wasm.clone(),
                ]);
            }
            let client = CodexAppServerClient::launch(CodexAppServerConfig {
                executable,
                executable_sha256,
                implementation_version: "installed".into(),
                workspace: self.config.workspace.clone(),
                codex_home: session_root.join("codex-home"),
                scratch_directory: session_root.join("scratch"),
                auth_file: self.config.codex_auth_file.clone(),
                brokered_mcp_server: mcp.map(|mcp| CodexMcpServerConfig {
                    executable: mcp.executable,
                    executable_sha256: mcp.executable_sha256,
                    arguments: mcp.arguments,
                }),
                containment: Some(AgentContainmentConfig {
                    proxy_url: endpoint.proxy_url.clone(),
                    credentialed_proxy_url: managed_proxy.credentialed_proxy_url().to_owned(),
                    allowed_hosts: endpoint.allowed_hosts.clone(),
                    allowed_unix_sockets,
                    read_paths,
                    write_paths: vec![session_root.to_path_buf()],
                }),
            })
            .map_err(|_| StartError::Driver)?;
            return Ok(LaunchedAgentProvider {
                client: Arc::new(client),
                managed_proxy: Some(managed_proxy),
            });
        }
        let provider = self
            .config
            .acp_providers
            .iter()
            .find(|provider| provider.provider_id == provider_id)
            .ok_or(StartError::Unavailable)?;
        let executable_sha256 =
            sha256_file(&provider.executable).map_err(|_| StartError::Driver)?;
        let allowed_hosts =
            acp_network_allowed_hosts(&provider.provider_id).ok_or(StartError::Unavailable)?;
        let managed_proxy =
            ManagedConnectProxy::start(allowed_hosts.iter().map(|host| (*host).to_owned()))
                .map_err(|_| StartError::Driver)?;
        let endpoint = managed_proxy.endpoint();
        let allowed_unix_sockets = mcp
            .as_ref()
            .map(|_| vec![self.config.control_socket.clone()])
            .unwrap_or_default();
        let mut read_paths = acp_provider_read_paths(provider);
        let mut environment = provider.environment.clone();
        if provider.provider_id == "codex-acp" {
            let isolated_home = session_root.join("home");
            let codex_home = session_root.join("codex-home");
            let scratch = session_root.join("scratch");
            for directory in [&isolated_home, &codex_home, &scratch] {
                create_private_runtime_root(directory).map_err(|_| StartError::Driver)?;
            }
            stage_codex_auth_file(self.config.codex_auth_file.as_deref(), &codex_home)
                .map_err(|_| StartError::Driver)?;
            if let Some(auth_file) = self.config.codex_auth_file.as_deref() {
                let mut seen = read_paths.iter().cloned().collect::<HashSet<_>>();
                add_existing_acp_read_path(&mut read_paths, &mut seen, auth_file);
            }
            if let Some(home) = provider.environment.get("HOME") {
                stage_acp_codex_preferences(Path::new(home), &codex_home)
                    .map_err(|_| StartError::Driver)?;
            }
            environment.insert("HOME".into(), isolated_home.into_os_string());
            environment.insert("CODEX_HOME".into(), codex_home.into_os_string());
            environment.insert("TMPDIR".into(), scratch.into_os_string());
        }
        let client = AcpAgentClient::launch(AcpAgentConfig {
            executable: provider.executable.clone(),
            executable_sha256,
            arguments: provider.arguments.clone(),
            environment,
            implementation_version: provider.implementation_version.clone(),
            provider_id: provider.provider_id.clone(),
            workspace: self.config.workspace.clone(),
            brokered_mcp_server: mcp.map(|mcp| AcpMcpServerConfig {
                executable: mcp.executable,
                executable_sha256: mcp.executable_sha256,
                arguments: mcp.arguments,
            }),
            containment: Some(AgentContainmentConfig {
                proxy_url: endpoint.proxy_url.clone(),
                credentialed_proxy_url: managed_proxy.credentialed_proxy_url().to_owned(),
                allowed_hosts: endpoint.allowed_hosts.clone(),
                allowed_unix_sockets,
                read_paths,
                write_paths: vec![session_root.to_path_buf()],
            }),
        })
        .map_err(|_| StartError::Driver)?;
        Ok(LaunchedAgentProvider {
            client: Arc::new(client),
            managed_proxy: Some(managed_proxy),
        })
    }

    fn snapshot(&self, session_id: u16) -> Result<AgentSnapshotResponse, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        let status = progress.status;
        let turn_id = progress.turn_id.clone();
        let error = progress.error.clone();
        drop(progress);
        let document = self
            .config
            .daemon
            .block_snapshot(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        let capabilities = session
            .client
            .session_capabilities()
            .map_err(|_| SessionError::Driver)?;
        Ok(AgentSnapshotResponse {
            session_id,
            status,
            turn_id,
            error,
            capabilities,
            document,
        })
    }

    fn stream_status(&self, session_id: u16) -> Result<AgentStatus, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        Ok(progress.status)
    }

    fn stream_state(&self, session_id: u16) -> Result<AgentStreamStateFrame, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        let status = progress.status;
        let turn_id = progress.turn_id.clone();
        let error = progress.error.clone();
        drop(progress);
        let capabilities = session
            .client
            .session_capabilities()
            .map_err(|_| SessionError::Driver)?;
        Ok(AgentStreamStateFrame {
            status,
            turn_id,
            error,
            capabilities,
        })
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
                AgentStatus::Running | AgentStatus::WaitingApproval
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
                AgentStatus::Running | AgentStatus::WaitingApproval
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
            if request.decision == PermissionDecision::AllowOnce
                && workspace_apply.is_none()
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

    fn mcp_launch(
        &self,
        task_id: TaskId,
        session_root: &std::path::Path,
    ) -> Option<Result<BrokeredMcpLaunch, StartError>> {
        let executable = match self.config.mcp_executable.as_ref()?.canonicalize() {
            Ok(executable) => executable,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let digest = match sha256_file(&executable) {
            Ok(digest) => digest,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let mut arguments = vec![
            "--agent-mode".into(),
            "--socket".into(),
            self.config.control_socket.clone().into_os_string(),
            "--task-id".into(),
            task_id.to_string().into(),
        ];
        if let Some(runtime) = &self.config.genui_runtime {
            let deno_sha256 = match sha256_file(&runtime.deno_executable) {
                Ok(digest) => digest,
                Err(_) => return Some(Err(StartError::Driver)),
            };
            let script_sha256 = match sha256_file(&runtime.compiler_script) {
                Ok(digest) => digest,
                Err(_) => return Some(Err(StartError::Driver)),
            };
            let wasm_sha256 = match sha256_file(&runtime.compiler_wasm) {
                Ok(digest) => digest,
                Err(_) => return Some(Err(StartError::Driver)),
            };
            let deno_root = session_root.join("deno-tools");
            arguments.extend([
                "--deno".into(),
                runtime.deno_executable.clone().into_os_string(),
                "--deno-sha256".into(),
                deno_sha256.into(),
                "--deno-version".into(),
                runtime.runtime_version.clone().into(),
                "--deno-cache".into(),
                deno_root.join("cache").into_os_string(),
                "--deno-scratch".into(),
                deno_root.join("scratch").into_os_string(),
            ]);
            if let Ok(snapshot) = create_workspace_snapshot(
                &self.config.workspace,
                &deno_root.join("workspace-snapshot"),
            ) {
                arguments.extend([
                    "--workspace-snapshot".into(),
                    snapshot.root.into_os_string(),
                ]);
            }
            arguments.extend([
                "--genui-script".into(),
                runtime.compiler_script.clone().into_os_string(),
                "--genui-script-sha256".into(),
                script_sha256.into(),
                "--genui-wasm".into(),
                runtime.compiler_wasm.clone().into_os_string(),
                "--genui-wasm-sha256".into(),
                wasm_sha256.into(),
                "--genui-compiler-version".into(),
                runtime.compiler_version.clone().into(),
            ]);
        }
        Some(Ok(BrokeredMcpLaunch {
            executable,
            executable_sha256: digest,
            arguments,
        }))
    }

    fn close_session(&self, session_id: u16) {
        self.close_artifact_drafts(session_id);
        self.close_workspace_applies(session_id);
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id));
        if let Some(session) = session {
            let _ = session.client.close();
            let _ = std::fs::remove_dir_all(&session.runtime_root);
        }
        if let Some(editor_lsp) = &self.editor_lsp {
            editor_lsp.close_session(session_id);
        }
    }

    fn close_all(&self) {
        let session_ids = self
            .sessions
            .lock()
            .map(|sessions| sessions.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for session_id in session_ids {
            self.close_artifact_drafts(session_id);
            self.close_workspace_applies(session_id);
        }
        let sessions = if let Ok(mut sessions) = self.sessions.lock() {
            sessions.drain().map(|(_, session)| session).collect()
        } else {
            Vec::new()
        };
        for session in sessions {
            let _ = session.client.close();
            let _ = std::fs::remove_dir_all(&session.runtime_root);
        }
        if let Some(editor_lsp) = &self.editor_lsp {
            editor_lsp.close_all();
        }
        if let Some(compiler) = &self.artifact_draft_compiler {
            compiler.close();
        }
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
        || draft.files.len() > MAX_ARTIFACT_DRAFT_FILES
        || !draft.files.contains_key(&draft.entrypoint)
        || !draft.files.keys().eq(artifact.source_files.keys())
    {
        return Err(ArtifactDraftError::InvalidRequest);
    }
    let source_bytes = draft
        .files
        .values()
        .try_fold(0_usize, |total, source| total.checked_add(source.len()));
    if source_bytes.is_none_or(|bytes| bytes > MAX_ARTIFACT_DRAFT_SOURCE_BYTES) {
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
        ArtifactEditorStoreError::Io(_) | ArtifactEditorStoreError::Json(_) => {
            ArtifactEditorError::Store
        }
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
    let OperationAction::Opaque { kind, .. } = &operation.action else {
        return false;
    };
    matches!(
        kind.as_str(),
        "hyper_term.diff.review" | "hyper_term.lsp.query" | "hyper_term.genui.compile"
    )
}

fn run_turn(session: Arc<AgentSession>, daemon: DaemonState, prompt: String) {
    let turn_id = match session
        .client
        .start_turn(&session.thread_id, &prompt, START_TURN_TIMEOUT)
    {
        Ok(turn_id) => turn_id,
        Err(error) => {
            set_progress_failed(&session, &bounded_error(&error.to_string()));
            return;
        }
    };
    if let Ok(mut progress) = session.progress.lock() {
        progress.turn_id = Some(turn_id.clone());
    } else {
        let _ = session.client.close();
        return;
    }

    continue_turn(
        session,
        daemon,
        AgentTurnProjection {
            turn_id,
            agent_block_id: BlockId::new(),
            agent_message_phase: 0,
            plan_block_id: BlockId::new(),
            agent_message_bytes: 0,
            agent_message_interrupted: false,
            plan_bytes: 0,
        },
    );
}

fn continue_turn(
    session: Arc<AgentSession>,
    daemon: DaemonState,
    mut projection: AgentTurnProjection,
) {
    let deadline = Instant::now() + COMPLETE_TURN_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            set_progress_failed(&session, "Agent turn exceeded its five-minute bound");
            let _ = session.client.close();
            return;
        }
        let event = match session.client.next_event(remaining) {
            Ok(event) => event,
            Err(error) => {
                set_progress_failed(&session, &bounded_error(&error.to_string()));
                return;
            }
        };
        match event {
            AgentDriverEvent::MessageDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                if text.is_empty() {
                    continue;
                }
                if projection.agent_message_interrupted && projection.agent_message_bytes > 0 {
                    projection.agent_message_phase =
                        projection.agent_message_phase.saturating_add(1);
                    projection.agent_block_id = BlockId::new();
                }
                projection.agent_message_bytes = match projection
                    .agent_message_bytes
                    .checked_add(text.len())
                {
                    Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                    _ => {
                        set_progress_failed(&session, "Agent response exceeded its 256 KiB bound");
                        let _ = session.client.close();
                        return;
                    }
                };
                if daemon
                    .append_message(
                        session.task_id,
                        projection.agent_block_id,
                        MessageRole::Agent,
                        Some(format!(
                            "{}-message-{}",
                            projection.turn_id, projection.agent_message_phase
                        )),
                        text,
                    )
                    .is_err()
                {
                    set_progress_failed(&session, "Agent response could not be journaled");
                    let _ = session.client.close();
                    return;
                }
                projection.agent_message_interrupted = false;
            }
            AgentDriverEvent::PlanDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if text.is_empty() {
                    continue;
                }
                projection.plan_bytes = match projection.plan_bytes.checked_add(text.len()) {
                    Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                    _ => continue,
                };
                let _ = daemon.append_message(
                    session.task_id,
                    projection.plan_block_id,
                    MessageRole::Thought,
                    Some(projection.turn_id.clone()),
                    text,
                );
            }
            AgentDriverEvent::PlanUpdated {
                thread_id,
                turn_id: event_turn_id,
                entries,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if daemon
                    .update_agent_plan(session.task_id, projection.turn_id.clone(), entries)
                    .is_err()
                {
                    set_progress_failed(&session, "Agent plan could not be journaled");
                    let _ = session.client.close();
                    return;
                }
            }
            AgentDriverEvent::ToolCallUpdated {
                thread_id,
                turn_id: event_turn_id,
                call,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if daemon
                    .update_agent_tool_call(session.task_id, projection.turn_id.clone(), call)
                    .is_err()
                {
                    set_progress_failed(&session, "Agent tool call could not be journaled");
                    let _ = session.client.close();
                    return;
                }
            }
            AgentDriverEvent::ThoughtDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                if text.is_empty() {
                    continue;
                }
                projection.plan_bytes = match projection.plan_bytes.checked_add(text.len()) {
                    Some(total) if total <= MAX_AGENT_MESSAGE_BYTES => total,
                    _ => continue,
                };
                let _ = daemon.append_message(
                    session.task_id,
                    projection.plan_block_id,
                    MessageRole::Thought,
                    Some(projection.turn_id.clone()),
                    text,
                );
            }
            AgentDriverEvent::EffectProposed { proposal, .. } => {
                projection.agent_message_interrupted |= projection.agent_message_bytes > 0;
                let (kind, risk) = operation_kind_and_risk(proposal.kind);
                let operation = match daemon.propose_operation(
                    session.task_id,
                    kind,
                    OperationAction::Opaque {
                        kind: proposal.method.clone(),
                        payload_digest: proposal.payload_sha256.clone(),
                    },
                    proposal.summary.clone(),
                    risk,
                    proposal.required_capabilities.clone(),
                ) {
                    Ok(operation) => operation,
                    Err(_) => {
                        set_progress_failed(
                            &session,
                            "Agent effect proposal could not be journaled",
                        );
                        return;
                    }
                };
                let mut pending = match session.pending_effect.lock() {
                    Ok(pending) => pending,
                    Err(_) => {
                        set_progress_failed(
                            &session,
                            "Agent effect proposal could not be retained",
                        );
                        return;
                    }
                };
                if pending.is_some() {
                    set_progress_failed(&session, "Agent emitted overlapping effect proposals");
                    let _ = session.client.close();
                    return;
                }
                *pending = Some(PendingAgentEffect {
                    request_id: proposal.request_id,
                    payload_sha256: proposal.payload_sha256,
                    operation_id: operation.operation_id,
                    operation_revision: operation.revision,
                    projection,
                });
                drop(pending);
                if let Ok(mut progress) = session.progress.lock() {
                    progress.status = AgentStatus::WaitingApproval;
                }
                return;
            }
            AgentDriverEvent::TurnCompleted {
                thread_id,
                turn_id: event_turn_id,
                status,
                ..
            } if thread_id == session.thread_id
                && event_turn_id
                    .as_deref()
                    .is_none_or(|value| value == projection.turn_id) =>
            {
                if status.as_deref() == Some("failed") {
                    set_progress_failed(&session, "Agent reported a failed turn");
                } else if let Ok(mut progress) = session.progress.lock() {
                    progress.status = AgentStatus::Completed;
                    progress.error = None;
                }
                return;
            }
            AgentDriverEvent::Exited { .. } => {
                set_progress_failed(&session, "Agent exited before the turn completed");
                return;
            }
            _ => {}
        }
    }
}

fn operation_kind_and_risk(kind: AgentEffectKind) -> (OperationKind, RiskClass) {
    match kind {
        AgentEffectKind::Shell => (
            OperationKind::Other("agent_shell".into()),
            RiskClass::ExternalEffect,
        ),
        AgentEffectKind::WorkspaceEdit => (OperationKind::FileEdit, RiskClass::WorkspaceWrite),
        AgentEffectKind::Tool => (OperationKind::AgentTool, RiskClass::ExternalEffect),
        AgentEffectKind::ComputerUse => (OperationKind::ComputerUse, RiskClass::ExternalEffect),
        AgentEffectKind::Opaque => (
            OperationKind::Other("agent_effect".into()),
            RiskClass::ExternalEffect,
        ),
    }
}

fn set_progress_failed(session: &AgentSession, message: &str) {
    if let Ok(mut progress) = session.progress.lock() {
        progress.status = AgentStatus::Failed;
        progress.error = Some(bounded_error(message));
    }
}

fn bounded_error(message: &str) -> String {
    let mut end = message.len().min(512);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

fn ready_response(session_id: u16, session: &AgentSession) -> AgentSessionResponse {
    AgentSessionResponse {
        session_id,
        provider: session.provider_id.clone(),
        protocol: structured_protocol_name(session.protocol).into(),
        status: "ready",
        task_id: session.task_id,
        thread_id: session.thread_id.clone(),
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
    use std::os::unix::fs::PermissionsExt;

    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn draft_fixture() -> crate::artifact_store::StoredGenUiArtifact {
        crate::artifact_store::StoredGenUiArtifact {
            metadata: hyper_term_protocol::AcceptedGenUiArtifact {
                artifact_id: ArtifactId::new(),
                source_revision: 7,
                entrypoint: "/App.tsx".into(),
                content_digest: "a".repeat(64),
                compiler: hyper_term_protocol::GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
            },
            source_files: BTreeMap::from([
                ("/App.tsx".into(), "export default () => null;".into()),
                ("/theme.ts".into(), "export const accent = 'green';".into()),
            ]),
            bundle: "globalThis.fixture=true;".into(),
            css: String::new(),
            source_map: "{}".into(),
        }
    }

    fn capsule_fixture() -> GenUiBugCapsule {
        let artifact = draft_fixture();
        let editor = ArtifactEditorCheckpoint {
            schema_version: 1,
            artifact_id: artifact.metadata.artifact_id,
            base_source_revision: artifact.metadata.source_revision,
            revision: 0,
            state_digest: "b".repeat(64),
            entrypoint: artifact.metadata.entrypoint.clone(),
            files: artifact.source_files.clone(),
            active_path: artifact.metadata.entrypoint.clone(),
            view: crate::artifact_editor_store::ArtifactEditorView::Trace,
            selections: BTreeMap::new(),
        };
        let runtime = GenUiRuntimeTraceProjection {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            projection_digest: "c".repeat(64),
            events: Vec::new(),
        };
        build_bug_capsule(
            &artifact,
            &editor,
            &runtime,
            GenUiBugCapsuleEnvironment {
                hyper_term_version: "0.1.0".into(),
                os: "macos".into(),
                architecture: "aarch64".into(),
                deno_runtime_version: None,
                deno_executable_digest: None,
                compiler_script_digest: None,
                compiler_wasm_digest: None,
            },
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn acp_containment_reads_only_the_adapter_provider_and_auth_roots() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("runtime");
        let adapter_root = runtime.join("acp/node_modules");
        let adapter = adapter_root.join("@agentclientprotocol/codex-acp/dist/index.js");
        let provider_root = temporary.path().join("provider/node_modules");
        let provider = provider_root.join("@openai/codex/bin/codex.js");
        let executable = runtime.join("deno");
        let bin = temporary.path().join("bin");
        let node_root = temporary.path().join("Cellar/node/26.0.0");
        let node = node_root.join("bin/node");
        let home = temporary.path().join("home");
        let codex_root = home.join(".codex");
        let auth = codex_root.join("auth.json");
        let unrelated = home.join("Documents/private.txt");
        for directory in [
            adapter.parent().unwrap(),
            provider.parent().unwrap(),
            &bin,
            node.parent().unwrap(),
            &codex_root,
            unrelated.parent().unwrap(),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for file in [&adapter, &provider, &auth, &unrelated] {
            std::fs::write(file, "fixture").unwrap();
        }
        std::fs::write(&executable, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(&node, "node fixture").unwrap();
        symlink(&node, bin.join("node")).unwrap();
        let provider = AcpAgentProviderConfig {
            provider_id: "codex-acp".into(),
            executable,
            arguments: vec!["run".into(), adapter.into_os_string()],
            environment: BTreeMap::from([
                ("HOME".into(), home.clone().into_os_string()),
                ("PATH".into(), bin.into_os_string()),
                ("CODEX_PATH".into(), provider.into_os_string()),
            ]),
            implementation_version: "fixture-1".into(),
        };

        let paths = acp_provider_read_paths(&provider);
        assert!(paths.contains(&adapter_root.canonicalize().unwrap()));
        assert!(paths.contains(&provider_root.canonicalize().unwrap()));
        assert!(paths.contains(&node_root.canonicalize().unwrap()));
        assert!(paths.contains(&auth.canonicalize().unwrap()));
        assert!(!paths.contains(&codex_root.canonicalize().unwrap()));
        assert!(!paths.contains(&home.canonicalize().unwrap()));
        assert!(!paths.contains(&unrelated.canonicalize().unwrap()));
        assert_eq!(
            acp_network_allowed_hosts("codex-acp").unwrap(),
            CODEX_NETWORK_ALLOWED_HOSTS
        );
    }

    #[test]
    fn daemon_restart_reconciles_a_durable_workspace_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let daemon_state = temporary.path().join("daemon-state");
        let gateway_state = temporary.path().join("gateway-state");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&gateway_state).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("App.tsx"), "before\n").unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let daemon = DaemonState::open(&daemon_state).unwrap();
        let (task_id, operation_id, dispatching) = workspace_dispatch(&daemon);
        let set =
            prepare_workspace_apply_set(&workspace, vec![("App.tsx".into(), "after\n".into())])
                .unwrap();
        let result = apply_workspace_set_plan_durable(
            &workspace,
            &gateway_state,
            WorkspaceTransactionContext {
                task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &set,
        )
        .unwrap();
        assert!(matches!(result, DurableWorkspaceApplyResult::Committed(_)));
        drop(daemon);

        let daemon = DaemonState::open(&daemon_state).unwrap();
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            OperationState::UnknownExecution
        );
        let recovery = recover_workspace_transactions(&workspace, &gateway_state).unwrap();
        assert!(reconcile_workspace_recovery(&daemon, &gateway_state, recovery).is_none());
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            OperationState::Succeeded
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            "after\n"
        );
        assert!(
            std::fs::read_dir(gateway_state.join("workspace-transactions"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn daemon_restart_reconciles_a_safely_rolled_back_workspace_apply() {
        let temporary = tempfile::tempdir().unwrap();
        let daemon_state = temporary.path().join("daemon-state");
        let gateway_state = temporary.path().join("gateway-state");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&gateway_state).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("one.ts"), "one before\n").unwrap();
        std::fs::write(workspace.join("two.ts"), "two before\n").unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let daemon = DaemonState::open(&daemon_state).unwrap();
        let (task_id, operation_id, dispatching) = workspace_dispatch(&daemon);
        let set = prepare_workspace_apply_set(
            &workspace,
            vec![
                ("one.ts".into(), "one after\n".into()),
                ("two.ts".into(), "two after\n".into()),
            ],
        )
        .unwrap();
        std::fs::write(workspace.join("two.ts"), "external writer\n").unwrap();
        let result = apply_workspace_set_plan_durable(
            &workspace,
            &gateway_state,
            WorkspaceTransactionContext {
                task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &set,
        )
        .unwrap();
        assert!(matches!(result, DurableWorkspaceApplyResult::RolledBack(_)));
        drop(daemon);

        let daemon = DaemonState::open(&daemon_state).unwrap();
        let recovery = recover_workspace_transactions(&workspace, &gateway_state).unwrap();
        assert!(reconcile_workspace_recovery(&daemon, &gateway_state, recovery).is_none());
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            OperationState::Failed
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("one.ts")).unwrap(),
            "one before\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("two.ts")).unwrap(),
            "external writer\n"
        );
    }

    fn workspace_dispatch(
        daemon: &DaemonState,
    ) -> (TaskId, OperationId, hyper_term_core::OperationRecord) {
        let task_id = daemon.create_task("workspace recovery".into()).unwrap();
        let proposed = daemon
            .propose_operation(
                task_id,
                OperationKind::FileEdit,
                OperationAction::Opaque {
                    kind: "hyper_term.workspace.apply".into(),
                    payload_digest: "a".repeat(64),
                },
                "apply artifact".into(),
                RiskClass::WorkspaceWrite,
                vec!["workspace_write".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                proposed.operation_id,
                proposed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let dispatching = daemon
            .begin_operation(task_id, proposed.operation_id, authorized.revision)
            .unwrap();
        (task_id, proposed.operation_id, dispatching)
    }

    #[test]
    fn artifact_drafts_require_the_current_revision_and_rust_owned_file_set() {
        let artifact = draft_fixture();
        let changed = BTreeMap::from([
            (
                "/App.tsx".into(),
                "export default () => <main>Live</main>;".into(),
            ),
            ("/theme.ts".into(), "export const accent = 'green';".into()),
        ]);
        let request = validate_artifact_draft(
            &artifact,
            AgentArtifactDraftRequest {
                base_source_revision: 7,
                entrypoint: "/App.tsx".into(),
                files: changed.clone(),
            },
        )
        .unwrap();
        assert_eq!(request.source_revision, 8);
        assert_eq!(request.files, changed);
        assert!(matches!(
            validate_artifact_draft(
                &artifact,
                AgentArtifactDraftRequest {
                    base_source_revision: 6,
                    entrypoint: "/App.tsx".into(),
                    files: request.files.clone(),
                }
            ),
            Err(ArtifactDraftError::StaleRevision)
        ));
        let mut escaped = request.files;
        escaped.insert("/invented.ts".into(), "export {};".into());
        assert!(matches!(
            validate_artifact_draft(
                &artifact,
                AgentArtifactDraftRequest {
                    base_source_revision: 7,
                    entrypoint: "/App.tsx".into(),
                    files: escaped,
                }
            ),
            Err(ArtifactDraftError::InvalidRequest)
        ));
        assert!(matches!(
            validate_artifact_draft(
                &artifact,
                AgentArtifactDraftRequest {
                    base_source_revision: 7,
                    entrypoint: "/App.tsx".into(),
                    files: artifact.source_files.clone(),
                }
            ),
            Err(ArtifactDraftError::NoChanges)
        ));
    }

    #[test]
    fn base_fenced_acceptance_rejects_an_artifact_replaced_during_build() {
        let temporary = tempfile::tempdir().unwrap();
        let daemon_state = temporary.path().join("daemon-state");
        let daemon = DaemonState::open(&daemon_state).unwrap();
        let task_id = daemon.create_task("artifact base fence".into()).unwrap();
        let dispatch = || {
            let proposed = daemon
                .propose_operation(
                    task_id,
                    OperationKind::McpTool,
                    OperationAction::Opaque {
                        kind: "hyper_term.genui.compile".into(),
                        payload_digest: "a".repeat(64),
                    },
                    "compile artifact".into(),
                    RiskClass::ReadOnly,
                    vec!["artifact_build".into()],
                )
                .unwrap();
            let authorized = daemon
                .decide_permission(
                    task_id,
                    proposed.operation_id,
                    proposed.revision,
                    PermissionDecision::AllowOnce,
                )
                .unwrap();
            daemon
                .begin_operation(task_id, proposed.operation_id, authorized.revision)
                .unwrap()
        };
        let candidate = |revision: u64, label: &str| {
            let bundle = format!("globalThis.label={label:?};");
            GenUiArtifactCandidate {
                schema_version: 1,
                source_revision: revision,
                entrypoint: "/App.tsx".into(),
                source_files: BTreeMap::from([(
                    "/App.tsx".into(),
                    format!("export default () => {label:?};"),
                )]),
                content_digest: sha256_bytes(bundle.as_bytes()),
                bundle,
                css: String::new(),
                source_map: "{}".into(),
                compiler: hyper_term_protocol::GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
                diagnostics: Vec::new(),
            }
        };
        let first_operation = dispatch();
        let first = daemon
            .accept_genui_artifact(
                task_id,
                first_operation.operation_id,
                first_operation.revision,
                candidate(1, "first"),
            )
            .unwrap();
        let second_operation = dispatch();
        let second = daemon
            .accept_genui_artifact_from_base(
                task_id,
                second_operation.operation_id,
                second_operation.revision,
                first.artifact_id,
                first.source_revision,
                candidate(2, "second"),
            )
            .unwrap();
        let stale_operation = dispatch();
        assert!(matches!(
            daemon.accept_genui_artifact_from_base(
                task_id,
                stale_operation.operation_id,
                stale_operation.revision,
                first.artifact_id,
                first.source_revision,
                candidate(2, "stale"),
            ),
            Err(crate::DaemonError::ArtifactBaseNotCurrent { .. })
        ));
        assert_eq!(
            daemon.active_genui_artifact(task_id).unwrap().unwrap(),
            second
        );
        let history = daemon
            .genui_artifact_history(task_id, second.artifact_id)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].artifact, second);
        assert_eq!(history[1].artifact, first);
        assert!(history[0].event_sequence > history[1].event_sequence);
        assert_eq!(
            daemon
                .read_genui_artifact_revision(task_id, second.artifact_id, first.artifact_id)
                .unwrap()
                .source_files["/App.tsx"],
            "export default () => \"first\";"
        );

        drop(daemon);
        let reopened = DaemonState::open(&daemon_state).unwrap();
        let reopened_history = reopened
            .genui_artifact_history(task_id, second.artifact_id)
            .unwrap();
        assert_eq!(reopened_history, history);
        assert_eq!(
            reopened
                .read_genui_artifact_revision(task_id, second.artifact_id, first.artifact_id)
                .unwrap()
                .metadata,
            first
        );
    }

    #[test]
    fn preview_bootstrap_is_inline_escaped_and_keeps_the_runtime_capsule() {
        let artifact_id = ArtifactId::new();
        let stored = crate::artifact_store::StoredGenUiArtifact {
            metadata: hyper_term_protocol::AcceptedGenUiArtifact {
                artifact_id,
                source_revision: 3,
                entrypoint: "/App.tsx".into(),
                content_digest: "a".repeat(64),
                compiler: hyper_term_protocol::GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
            },
            source_files: BTreeMap::from([(
                "/App.tsx".into(),
                "export default () => null;".into(),
            )]),
            bundle: "globalThis.value='</script><script>bad()'".into(),
            css: "main::after{content:'<&>'}".into(),
            source_map: "{}".into(),
        };
        let shell = format!(
            "<html><head>{ARTIFACT_BOOTSTRAP_MARKER}<script>hyper_term_preview_boot</script></head></html>"
        );
        let document = render_preview_document(&shell, &stored).unwrap();
        assert!(!document.contains("</script><script>bad()"));
        assert!(document.contains("\\u003c/script\\u003e\\u003cscript\\u003ebad()"));
        assert!(document.contains(&artifact_id.to_string()));
        assert!(document.contains("hyper_term_preview_boot"));
        assert!(document.contains("\"source_map\":\"{}\""));
        assert!(!document.contains(ARTIFACT_BOOTSTRAP_MARKER));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_capsule_endpoint_requires_only_the_desktop_gateway_token() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let expected = capsule_fixture();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: DaemonState::open(temporary.path().join("daemon-state")).unwrap(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: Some(expected.clone()),
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let path = format!("/agent/debug-capsule?token={token}");
        let (status, body) = request_path(gateway.address(), &path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let actual: GenUiBugCapsule = serde_json::from_slice(&body).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/debug-capsule?token=wrong",
                "GET",
                b""
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_session_streams_a_turn_into_the_block_document() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"thread\":{\"id\":\"thread-3\"}}}' ;;\n    *'\"method\":\"turn/start\"'*)\n      printf '%s\\n' '{\"id\":3,\"result\":{\"turn\":{\"id\":\"turn-1\"}}}'\n      printf '%s\\n' '{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread-3\",\"turnId\":\"turn-1\",\"itemId\":\"message-1\",\"delta\":\"Hyper Term \"}}'\n      printf '%s\\n' '{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread-3\",\"turnId\":\"turn-1\",\"itemId\":\"message-1\",\"delta\":\"Agent is live.\"}}'\n      printf '%s\\n' '{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread-3\",\"turn\":{\"id\":\"turn-1\",\"status\":\"completed\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex)
            .expect("fake Codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).expect("fake Codex executable");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace,
            state_directory: state,
            daemon,
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        assert_eq!(
            request(gateway.address(), "wrong-token", 3, "POST").await.0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let (status, body) = request(gateway.address(), &token, 3, "POST").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).expect("start response");
        assert_eq!(response["provider"], "codex");
        assert_eq!(response["protocol"], "codex-app-server-v2");
        assert_eq!(response["status"], "ready");
        assert_eq!(response["thread_id"], "thread-3");

        let stream_response = stream_session(
            State(gateway.runtime.clone()),
            Query(AgentSessionQuery {
                token: Some(token.clone()),
                session_id: Some(3),
                provider: None,
            }),
        )
        .await;
        assert_eq!(stream_response.status(), StatusCode::OK);
        assert_eq!(
            stream_response.headers()[CONTENT_TYPE],
            "application/x-ndjson; charset=utf-8"
        );
        assert_eq!(stream_response.headers()[CACHE_CONTROL], "no-store");
        let mut updates = stream_response.into_body().into_data_stream();
        let initial = tokio::time::timeout(Duration::from_secs(1), updates.next())
            .await
            .expect("initial stream timeout")
            .expect("initial stream frame")
            .expect("initial stream body");
        let initial: serde_json::Value =
            serde_json::from_slice(initial.as_ref()).expect("initial NDJSON state");
        assert_eq!(initial["type"], "state");
        assert_eq!(initial["status"], "ready");
        assert!(initial.get("document").is_none());

        let unrelated_task = gateway
            .runtime
            .config
            .daemon
            .create_task("unrelated stream task".into())
            .expect("create unrelated task");
        gateway
            .runtime
            .config
            .daemon
            .append_message(
                unrelated_task,
                BlockId::new(),
                MessageRole::Agent,
                None,
                "must not cross the session boundary".into(),
            )
            .expect("append unrelated message");
        assert!(
            tokio::time::timeout(Duration::from_millis(150), updates.next())
                .await
                .is_err(),
            "Agent stream must filter another task's block patches"
        );

        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session/turn?token={token}&session_id=3"),
            "POST",
            b"Reply with the live marker",
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");

        let mut saw_patch = false;
        loop {
            let body = tokio::time::timeout(Duration::from_secs(2), updates.next())
                .await
                .expect("Agent stream update timeout")
                .expect("Agent stream stayed open")
                .expect("Agent stream body");
            let frame: serde_json::Value =
                serde_json::from_slice(body.as_ref()).expect("NDJSON Agent stream frame");
            match frame["type"].as_str() {
                Some("patch") => {
                    saw_patch = true;
                    assert!(frame["patch"]["target_revision"].as_u64().is_some());
                    assert!(frame.get("document").is_none());
                }
                Some("state") if frame["status"] == "completed" => break,
                Some("state" | "resync") => {}
                other => panic!("unexpected Agent stream frame: {other:?}"),
            }
        }
        assert!(saw_patch, "Agent turn should emit canonical block patches");
        let (status, body) = request(gateway.address(), &token, 3, "GET").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let snapshot: serde_json::Value =
            serde_json::from_slice(&body).expect("final snapshot response");
        let blocks = snapshot["document"]["blocks"]
            .as_array()
            .expect("snapshot blocks");
        assert!(blocks.iter().any(|block| {
            block["payload"]["role"] == "user"
                && block["payload"]["text"] == "Reply with the live marker"
        }));
        assert!(blocks.iter().any(|block| {
            block["payload"]["role"] == "agent"
                && block["payload"]["text"] == "Hyper Term Agent is live."
        }));

        assert_eq!(
            request(gateway.address(), &token, 3, "DELETE").await.0,
            StatusCode::NO_CONTENT.as_u16()
        );
        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn configured_acp_provider_uses_the_same_agent_session_projection() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let gateway_state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"acp-session-8\",\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"fast\",\"options\":[{\"value\":\"fast\",\"name\":\"Fast\"},{\"value\":\"deep\",\"name\":\"Deep\"}]}]}}' ;;\n    *'\"method\":\"session/set_config_option\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"deep\",\"options\":[{\"value\":\"fast\",\"name\":\"Fast\"},{\"value\":\"deep\",\"name\":\"Deep\"}]}]}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"available_commands_update\",\"availableCommands\":[{\"name\":\"skills\",\"description\":\"Configure skills\"}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Provider-neutral ACP is live.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Checking workspace\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Final answer.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        )
        .expect("fake ACP");
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: gateway_state.clone(),
            daemon,
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session?token={token}&session_id=8&provider=fixture-acp"),
            "POST",
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["provider"], "fixture-acp");
        assert_eq!(response["protocol"], "acp-v1");
        assert_eq!(response["thread_id"], "acp-session-8");
        let (status, body) = request(gateway.address(), &token, 8, "GET").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot["capabilities"]["config_options"][0]["id"], "model");
        assert_eq!(
            snapshot["capabilities"]["config_options"][0]["kind"]["current_value"],
            "fast"
        );
        let config_path = format!("/agent/session/config?token={token}&session_id=8");
        let config_request = serde_json::to_vec(&serde_json::json!({
            "config_id": "model",
            "value": {"type": "id", "value": "deep"}
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &config_path, "POST", &config_request).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            response["capabilities"]["config_options"][0]["kind"]["current_value"],
            "deep"
        );
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session?token={token}&session_id=8&provider=codex"),
                "POST",
                b"",
            )
            .await
            .0,
            StatusCode::CONFLICT.as_u16()
        );

        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/turn?token={token}&session_id=8"),
                "POST",
                b"Use ACP",
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let snapshot = loop {
            let (status, body) = request(gateway.address(), &token, 8, "GET").await;
            assert_eq!(status, StatusCode::OK.as_u16());
            let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if snapshot["status"] == "completed" {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let blocks = snapshot["document"]["blocks"]
            .as_array()
            .expect("snapshot blocks");
        assert_eq!(
            snapshot["capabilities"]["available_commands"][0]["name"],
            "skills"
        );
        let initial_message = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "agent"
                    && block["payload"]["text"] == "Provider-neutral ACP is live."
            })
            .expect("initial Agent message");
        let thought = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "thought"
                    && block["payload"]["text"] == "Checking workspace"
            })
            .expect("Agent thought");
        let final_message = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "agent" && block["payload"]["text"] == "Final answer."
            })
            .expect("final Agent message");
        assert!(initial_message < thought);
        assert!(thought < final_message);
        assert_eq!(
            std::fs::read_dir(gateway_state.join("agents"))
                .unwrap()
                .count(),
            1
        );
        gateway.shutdown().await.unwrap();
        assert_eq!(
            std::fs::read_dir(gateway_state.join("agents"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn acp_artifact_workspace_apply_set_waits_for_one_exact_approval() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace_source = concat!(
            "export default function App() {\n",
            "  const title = 'Workspace title';\n",
            "  const keepOne = 1;\n",
            "  const keepTwo = 2;\n",
            "  const keepThree = 3;\n",
            "  const keepFour = 4;\n",
            "  const keepFive = 5;\n",
            "  const keepSix = 6;\n",
            "  const keepSeven = 7;\n",
            "  const keepEight = 8;\n",
            "  const footer = 'Workspace footer';\n",
            "  return <main>{title}{footer}</main>;\n",
            "}\n",
        );
        let workspace_theme = "export const accent = 'workspace';\n";
        std::fs::write(workspace.join("App.tsx"), workspace_source).unwrap();
        std::fs::write(workspace.join("theme.ts"), workspace_theme).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"workspace-apply-session\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace: workspace.clone(),
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let session_path =
            format!("/agent/session?token={token}&session_id=10&provider=fixture-acp");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(session["task_id"].clone()).unwrap();

        let seed = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Seed workspace apply fixture".into(),
                RiskClass::ReadOnly,
                vec!["artifact_build".into()],
            )
            .unwrap();
        let seed = daemon
            .decide_permission(
                task_id,
                seed.operation_id,
                seed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let seed = daemon
            .begin_operation(task_id, seed.operation_id, seed.revision)
            .unwrap();
        let artifact_source = concat!(
            "export default function App() {\n",
            "  const title = 'AI title';\n",
            "  const keepOne = 1;\n",
            "  const keepTwo = 2;\n",
            "  const keepThree = 3;\n",
            "  const keepFour = 4;\n",
            "  const keepFive = 5;\n",
            "  const keepSix = 6;\n",
            "  const keepSeven = 7;\n",
            "  const keepEight = 8;\n",
            "  const footer = 'AI footer';\n",
            "  return <main>{title}{footer}</main>;\n",
            "}\n",
        );
        let artifact_theme = "export const accent = 'agent';\n";
        let bundle = "globalThis.workspaceApplyFixture=true;";
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                seed.operation_id,
                seed.revision,
                GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 3,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([
                        ("/App.tsx".into(), artifact_source.into()),
                        ("/theme.ts".into(), artifact_theme.into()),
                    ]),
                    bundle: bundle.into(),
                    css: String::new(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: sha256_bytes(bundle.as_bytes()),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        daemon
            .complete_operation(
                task_id,
                seed.operation_id,
                seed.revision,
                OperationCompletion {
                    executor: "fixture".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: "seeded workspace apply fixture".into(),
                    result_digest: Some(accepted.content_digest.clone()),
                },
            )
            .unwrap();

        let editor_state_path = format!(
            "/agent/artifact/{}/editor-state?token={token}&session_id=10",
            accepted.artifact_id
        );
        let (status, body) = request_path(gateway.address(), &editor_state_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let baseline_state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(baseline_state["revision"], 0);
        assert_eq!(baseline_state["active_path"], "/App.tsx");
        assert_eq!(baseline_state["files"]["/theme.ts"], artifact_theme);
        let edited_theme = "export const accent = 'draft amber';\n";
        let checkpoint = serde_json::to_vec(&serde_json::json!({
            "expected_revision": 0,
            "base_source_revision": 3,
            "files": {
                "/App.tsx": artifact_source,
                "/theme.ts": edited_theme
            },
            "active_path": "/theme.ts",
            "view": "diff",
            "selections": {
                "/theme.ts": {"anchor": 7, "head": 12}
            }
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &editor_state_path, "PUT", &checkpoint).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let saved_state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved_state["revision"], 1);
        assert_eq!(saved_state["state_digest"].as_str().map(str::len), Some(64));
        let (status, body) = request_path(gateway.address(), &editor_state_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let restored_state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(restored_state["files"]["/theme.ts"], edited_theme);
        assert_eq!(restored_state["active_path"], "/theme.ts");
        assert_eq!(restored_state["view"], "diff");
        assert_eq!(restored_state["selections"]["/theme.ts"]["head"], 12);
        assert_eq!(
            request_path(gateway.address(), &editor_state_path, "PUT", &checkpoint)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );

        let runtime_trace_path = format!(
            "/agent/artifact/{}/runtime-trace?token={token}&session_id=10",
            accepted.artifact_id
        );
        let trace_batch = serde_json::to_vec(&serde_json::json!({
            "source_revision": 3,
            "events": [
                {
                    "schema_version": 1,
                    "stream_id": "77777777-7777-4777-8777-777777777777",
                    "client_sequence": 1,
                    "kind": "checkpoint",
                    "name": "agent_status.changed",
                    "payload": {"expanded": true, "access_token": "must-not-persist"}
                },
                {
                    "schema_version": 1,
                    "stream_id": "77777777-7777-4777-8777-777777777777",
                    "client_sequence": 2,
                    "kind": "effect_receipt",
                    "name": "evidence.lookup",
                    "payload": {
                        "input": {"id": 7},
                        "outcome": "succeeded",
                        "output": {"passed": true}
                    }
                }
            ]
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &runtime_trace_path, "POST", &trace_batch).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(trace["source_revision"], 3);
        assert_eq!(trace["events"].as_array().unwrap().len(), 2);
        assert_eq!(trace["projection_digest"].as_str().map(str::len), Some(64));
        assert_eq!(trace["events"][0]["event_sequence"], 1);
        assert_eq!(trace["events"][0]["redacted"], true);
        assert_eq!(trace["events"][0]["payload"]["access_token"], "[REDACTED]");
        assert_eq!(
            trace["events"][0]["payload_digest"].as_str().map(str::len),
            Some(64)
        );
        let (status, body) =
            request_path(gateway.address(), &runtime_trace_path, "POST", &trace_batch).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let (status, body) = request_path(gateway.address(), &runtime_trace_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["events"][0]["name"],
            "agent_status.changed"
        );
        let stale_trace = serde_json::to_vec(&serde_json::json!({
            "source_revision": 2,
            "events": [{
                "schema_version": 1,
                "stream_id": "88888888-8888-4888-8888-888888888888",
                "client_sequence": 1,
                "kind": "action",
                "name": "stale.action",
                "payload": null
            }]
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &runtime_trace_path, "POST", &stale_trace)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );

        let capsule_path = format!(
            "/agent/artifact/{}/debug-capsule?token={token}&session_id=10",
            accepted.artifact_id
        );
        let (status, body) = request_path(gateway.address(), &capsule_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let encoded_capsule = String::from_utf8(body.to_vec()).unwrap();
        assert!(!encoded_capsule.contains("draft amber"));
        assert!(!encoded_capsule.contains("must-not-persist"));
        assert!(encoded_capsule.contains("[REDACTED]"));
        let capsule: GenUiBugCapsule = serde_json::from_str(&encoded_capsule).unwrap();
        assert_eq!(capsule.mode, "replay_only");
        assert_eq!(capsule.editor.files.len(), 2);
        assert!(capsule.editor.files.iter().any(|file| file.modified));
        assert_eq!(capsule.runtime.events.len(), 2);
        assert_eq!(capsule.capsule_digest.as_deref().map(str::len), Some(64));
        assert!(
            crate::artifact_debug_capsule::verify_bug_capsule(&capsule).unwrap(),
            "serialized capsule must verify after an offline parse"
        );
        assert!(capsule.inventory.iter().any(|entry| {
            entry.category == "terminal_output"
                && entry.inclusion == hyper_term_protocol::GenUiBugCapsuleInclusion::Excluded
        }));

        let preview_path = format!(
            "/agent/artifact/{}/workspace-preview?token={token}&session_id=10",
            accepted.artifact_id
        );
        let preview_request = serde_json::to_vec(&serde_json::json!({
            "artifact_source_revision": 3,
            "mappings": [
                {"source_path": "/App.tsx", "target_path": "App.tsx"},
                {"source_path": "/theme.ts", "target_path": "theme.ts"}
            ]
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &preview_path, "POST", &preview_request).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let preview: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(preview["artifact_source_revision"], 3);
        assert_eq!(preview["review_digest"].as_str().map(str::len), Some(64));
        assert_eq!(preview["changes"].as_array().unwrap().len(), 2);
        assert_eq!(preview["changes"][0]["source_path"], "/App.tsx");
        assert_eq!(preview["changes"][0]["before"], workspace_source);
        assert_eq!(preview["changes"][0]["artifact_after"], artifact_source);
        assert_eq!(preview["changes"][0]["hunks"].as_array().unwrap().len(), 2);
        assert_eq!(preview["changes"][1]["source_path"], "/theme.ts");
        assert_eq!(preview["changes"][1]["hunks"].as_array().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            workspace_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            workspace_theme
        );

        let app_hunk_id = preview["changes"][0]["hunks"][0]["id"].as_str().unwrap();
        let theme_hunk_id = preview["changes"][1]["hunks"][0]["id"].as_str().unwrap();
        let invalid_selection = serde_json::to_vec(&serde_json::json!({
            "artifact_source_revision": 3,
            "review_digest": preview["review_digest"],
            "mappings": [
                {
                    "source_path": "/App.tsx",
                    "target_path": "App.tsx",
                    "hunk_ids": ["0".repeat(64)]
                },
                {
                    "source_path": "/theme.ts",
                    "target_path": "theme.ts",
                    "hunk_ids": []
                }
            ]
        }))
        .unwrap();
        let apply_path = format!(
            "/agent/artifact/{}/workspace-apply?token={token}&session_id=10",
            accepted.artifact_id
        );
        assert_eq!(
            request_path(gateway.address(), &apply_path, "POST", &invalid_selection,)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY.as_u16()
        );

        let apply_request = serde_json::to_vec(&serde_json::json!({
            "artifact_source_revision": 3,
            "review_digest": preview["review_digest"],
            "mappings": [
                {
                    "source_path": "/App.tsx",
                    "target_path": "App.tsx",
                    "hunk_ids": [app_hunk_id]
                },
                {
                    "source_path": "/theme.ts",
                    "target_path": "theme.ts",
                    "hunk_ids": [theme_hunk_id]
                }
            ]
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &apply_path, "POST", &apply_request).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let proposal: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let selected_app_source = workspace_source.replacen("Workspace title", "AI title", 1);
        assert_eq!(proposal["status"], "waiting_approval");
        assert_eq!(proposal["before"], workspace_source);
        assert_eq!(proposal["after"], selected_app_source);
        assert_eq!(proposal["base_digest"].as_str().map(str::len), Some(64));
        assert_eq!(
            proposal["transaction_digest"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(proposal["changes"].as_array().unwrap().len(), 2);
        assert_eq!(proposal["changes"][1]["source_path"], "/theme.ts");
        assert_eq!(proposal["changes"][1]["before"], workspace_theme);
        assert_eq!(proposal["changes"][1]["after"], artifact_theme);
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            workspace_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            workspace_theme
        );
        let operation_id: OperationId =
            serde_json::from_value(proposal["operation_id"].clone()).unwrap();
        let operation = daemon.operation(operation_id).unwrap();
        assert_eq!(operation.kind, OperationKind::FileEdit);
        assert_eq!(operation.risk, RiskClass::WorkspaceWrite);
        assert!(matches!(
            operation.action,
            OperationAction::Opaque { ref kind, .. } if kind == "hyper_term.workspace.apply"
        ));

        let stale_approval = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": proposal["operation_revision"].as_u64().unwrap() + 1,
            "decision": "allow_once"
        }))
        .unwrap();
        let permission_path = format!("/agent/session/permission?token={token}&session_id=10");
        assert_eq!(
            request_path(gateway.address(), &permission_path, "POST", &stale_approval,)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            workspace_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            workspace_theme
        );

        let approval = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": proposal["operation_revision"],
            "decision": "allow_once"
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &permission_path, "POST", &approval)
                .await
                .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let status_path = format!(
            "{apply_path}&operation_id={}",
            proposal["operation_id"].as_str().unwrap()
        );
        let applied = loop {
            let (status, body) = request_path(gateway.address(), &status_path, "GET", b"").await;
            assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
            let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if response["status"] != "applying" {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            selected_app_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            artifact_theme
        );
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            hyper_term_protocol::OperationState::Succeeded
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
    async fn authenticated_acp_artifact_editor_queries_the_real_deno_lsp() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"editor-session\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let compiler_script = temporary.path().join("genui-compiler.js");
        let compiler_wasm = temporary.path().join("esbuild.wasm");
        let preview_shell = temporary.path().join("genui-preview.html");
        std::fs::write(&compiler_script, "compiler").unwrap();
        std::fs::write(&compiler_wasm, "wasm").unwrap();
        std::fs::write(
            &preview_shell,
            format!(
                "<html><head>{ARTIFACT_BOOTSTRAP_MARKER}<script>hyper_term_preview_boot</script></head></html>"
            ),
        )
        .unwrap();
        let deno =
            PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
                .canonicalize()
                .unwrap();
        assert_eq!(
            sha256_file(&deno).unwrap(),
            std::env::var("HYPER_TERM_DENO_SHA256").expect("HYPER_TERM_DENO_SHA256")
        );
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            mcp_executable: None,
            genui_runtime: Some(AgentGenUiRuntimeConfig {
                deno_executable: deno,
                runtime_version: "2.9.3".into(),
                compiler_script,
                compiler_wasm,
                preview_shell,
                compiler_version: "0.28.1".into(),
            }),
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session?token={token}&session_id=8&provider=fixture-acp"),
            "POST",
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(response["task_id"].clone()).unwrap();
        let proposed = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Compile editor LSP fixture".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                proposed.operation_id,
                proposed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let dispatching = daemon
            .begin_operation(task_id, proposed.operation_id, authorized.revision)
            .unwrap();
        let bundle = "globalThis.editorLsp = true;";
        let css = "";
        let mut digest = Sha256::new();
        digest.update(bundle.as_bytes());
        digest.update(css.as_bytes());
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                proposed.operation_id,
                dispatching.revision,
                hyper_term_protocol::GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 4,
                    entrypoint: "/main.ts".into(),
                    source_files: BTreeMap::from([(
                        "/main.ts".into(),
                        "const answer: string = 42;\n".into(),
                    )]),
                    bundle: bundle.into(),
                    css: css.into(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: digest
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        let lsp_path = format!(
            "/agent/artifact/{}/lsp?token={token}&session_id=8",
            accepted.artifact_id
        );
        let diagnostics = serde_json::to_vec(&serde_json::json!({
            "source_revision": 4,
            "document_path": "/main.ts",
            "source": "const answer: string = 42;\n",
            "kind": "diagnostics"
        }))
        .unwrap();
        let (status, body) = request_path(gateway.address(), &lsp_path, "POST", &diagnostics).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            response["diagnostics"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        let completion = serde_json::to_vec(&serde_json::json!({
            "source_revision": 4,
            "document_path": "/main.ts",
            "source": "const value = \"ok\";\nvalue.\n",
            "kind": "completion",
            "position": {"line": 1, "character": 6}
        }))
        .unwrap();
        let (status, body) = request_path(gateway.address(), &lsp_path, "POST", &completion).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["document_version"], 2);
        assert!(
            response["completions"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires HYPER_TERM_DENO_PATH and built dist/runtime GenUI assets"]
    async fn approved_artifact_draft_is_recompiled_by_deno_and_replaces_the_revision() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"draft-session\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let deno =
            PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
                .canonicalize()
                .unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            mcp_executable: None,
            genui_runtime: Some(AgentGenUiRuntimeConfig {
                deno_executable: deno,
                runtime_version: "2.9.3".into(),
                compiler_script: repository.join("dist/runtime/genui-compiler.js"),
                compiler_wasm: repository.join("dist/runtime/esbuild.wasm"),
                preview_shell: repository.join("dist/runtime/genui/preview.html"),
                compiler_version: "0.28.1".into(),
            }),
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let session_path =
            format!("/agent/session?token={token}&session_id=9&provider=fixture-acp");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(session["task_id"].clone()).unwrap();
        let initial_operation = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Seed draft fixture".into(),
                RiskClass::ReadOnly,
                vec!["artifact_build".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                initial_operation.operation_id,
                initial_operation.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let dispatching = daemon
            .begin_operation(task_id, initial_operation.operation_id, authorized.revision)
            .unwrap();
        let initial_source = "export default function App(){return <main>Initial</main>;}";
        let bundle = "globalThis.initialArtifact=true;";
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                initial_operation.operation_id,
                dispatching.revision,
                GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 1,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([("/App.tsx".into(), initial_source.into())]),
                    bundle: bundle.into(),
                    css: String::new(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: sha256_bytes(bundle.as_bytes()),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        daemon
            .complete_operation(
                task_id,
                initial_operation.operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "fixture".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: "seeded fixture".into(),
                    result_digest: Some(accepted.content_digest.clone()),
                },
            )
            .unwrap();
        let edited_source = "export default function App(){return <main>Published by Deno</main>;}";
        let draft_path = format!(
            "/agent/artifact/{}/draft?token={token}&session_id=9",
            accepted.artifact_id
        );
        let draft_body = serde_json::to_vec(&serde_json::json!({
            "base_source_revision": 1,
            "entrypoint": "/App.tsx",
            "files": {"/App.tsx": edited_source}
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &draft_path, "POST", &draft_body).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let rejected: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rejected_operation: OperationId =
            serde_json::from_value(rejected["operation_id"].clone()).unwrap();
        let rejection = serde_json::to_vec(&serde_json::json!({
            "operation_id": rejected_operation,
            "expected_revision": rejected["operation_revision"],
            "decision": "reject_once"
        }))
        .unwrap();
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/permission?token={token}&session_id=9"),
                "POST",
                &rejection,
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let rejected_path = format!("{draft_path}&operation_id={rejected_operation}");
        let (status, body) = request_path(gateway.address(), &rejected_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
            "rejected"
        );
        assert_eq!(
            daemon.active_genui_artifact(task_id).unwrap().unwrap(),
            accepted
        );
        let (status, body) =
            request_path(gateway.address(), &draft_path, "POST", &draft_body).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let proposed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(proposed["status"], "waiting_approval");
        let operation_id: OperationId =
            serde_json::from_value(proposed["operation_id"].clone()).unwrap();
        let operation_revision = proposed["operation_revision"].as_u64().unwrap();
        let snapshot = serde_json::to_string(&daemon.block_snapshot(task_id).unwrap()).unwrap();
        assert!(snapshot.contains("\"type\":\"approval\""));
        assert!(snapshot.contains(&operation_id.to_string()));
        let permission = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "decision": "allow_once"
        }))
        .unwrap();
        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=9"),
            "POST",
            &permission,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let status_path = format!("{draft_path}&operation_id={operation_id}");
        let mut published = None;
        for _ in 0..200 {
            let (status, body) = request_path(gateway.address(), &status_path, "GET", b"").await;
            assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
            let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if response["status"] == "accepted" {
                published = Some(response);
                break;
            }
            assert!(matches!(
                response["status"].as_str(),
                Some("waiting_approval" | "compiling")
            ));
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let published = published.expect("Deno artifact draft accepted within five seconds");
        let published_id = published["artifact"]["artifact_id"].as_str().unwrap();
        assert_eq!(published["artifact"]["source_revision"], 2);
        let source_path =
            format!("/agent/artifact/{published_id}/source?token={token}&session_id=9");
        let (status, body) = request_path(gateway.address(), &source_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let source: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(source["files"]["/App.tsx"], edited_source);
        assert_eq!(source["source_revision"], 2);
        let stale = serde_json::to_vec(&serde_json::json!({
            "base_source_revision": 1,
            "entrypoint": "/App.tsx",
            "files": {"/App.tsx": "export default () => null;"}
        }))
        .unwrap();
        let stale_path = format!("/agent/artifact/{published_id}/draft?token={token}&session_id=9");
        assert_eq!(
            request_path(gateway.address(), &stale_path, "POST", &stale)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accepted_artifact_preview_is_authenticated_current_and_network_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let gateway_state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"thread\":{\"id\":\"preview-thread\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).unwrap();

        let deno = temporary.path().join("deno");
        let compiler_script = temporary.path().join("genui-compiler.js");
        let compiler_wasm = temporary.path().join("esbuild.wasm");
        let preview_shell = temporary.path().join("genui-preview.html");
        std::fs::write(&deno, "deno").unwrap();
        std::fs::write(&compiler_script, "compiler").unwrap();
        std::fs::write(&compiler_wasm, "wasm").unwrap();
        std::fs::write(
            &preview_shell,
            format!(
                "<html><head>{ARTIFACT_BOOTSTRAP_MARKER}<script>hyper_term_preview_boot</script></head><body></body></html>"
            ),
        )
        .unwrap();
        let workbench_assets = temporary.path().join("workbench");
        std::fs::create_dir_all(workbench_assets.join("genui")).unwrap();
        std::fs::write(
            workbench_assets.join("index.html"),
            "<html><body>trusted-workbench<script src=\"index.js\"></script></body></html>",
        )
        .unwrap();
        std::fs::write(
            workbench_assets.join("index.js"),
            "globalThis.workbench=true;",
        )
        .unwrap();
        std::fs::write(
            workbench_assets.join("genui/preview.html"),
            "<html><script>globalThis.preview=true;</script></html>",
        )
        .unwrap();

        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: gateway_state,
            daemon: daemon.clone(),
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
            mcp_executable: None,
            genui_runtime: Some(AgentGenUiRuntimeConfig {
                deno_executable: deno,
                runtime_version: "2.9.3".into(),
                compiler_script,
                compiler_wasm,
                preview_shell,
                compiler_version: "0.28.1".into(),
            }),
            workbench_assets: Some(workbench_assets),
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        let (status, body) = request(gateway.address(), &token, 6, "POST").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(response["task_id"].clone()).unwrap();
        let workbench_path =
            format!("/agent/workbench/?token={token}&session_id=6&surface=artifact");
        let (status, headers, body) =
            request_path_raw(gateway.address(), &workbench_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-type: text/html"));
        assert!(headers.contains("connect-src 'self'"));
        assert!(headers.contains("'wasm-unsafe-eval'"));
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("trusted-workbench")
        );
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/workbench?token=wrong&session_id=6",
                "GET",
                b"",
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let (status, headers, body) =
            request_path_raw(gateway.address(), "/agent/workbench/index.js", "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert!(
            String::from_utf8(headers)
                .unwrap()
                .to_ascii_lowercase()
                .contains("content-type: text/javascript")
        );
        assert_eq!(body, b"globalThis.workbench=true;");
        let (status, headers, _) = request_path_raw(
            gateway.address(),
            "/agent/workbench/genui/preview.html",
            "GET",
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("frame-ancestors 'self'"));
        assert!(headers.contains("connect-src 'none'"));
        let proposed = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Compile preview fixture".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                proposed.operation_id,
                proposed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let dispatching = daemon
            .begin_operation(task_id, proposed.operation_id, authorized.revision)
            .unwrap();
        let bundle = "globalThis.__HYPER_PREVIEW_PROBE__ = 'ready';";
        let css = "main{color:#d7ff72}";
        let mut digest = Sha256::new();
        digest.update(bundle.as_bytes());
        digest.update(css.as_bytes());
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                proposed.operation_id,
                dispatching.revision,
                hyper_term_protocol::GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 9,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([(
                        "/App.tsx".into(),
                        "export default () => <main>ready</main>;".into(),
                    )]),
                    bundle: bundle.into(),
                    css: css.into(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: digest
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();

        let preview_path = format!(
            "/agent/artifact/{}/preview?token={token}&session_id=6",
            accepted.artifact_id
        );
        let (status, headers, body) =
            request_path_raw(gateway.address(), &preview_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-security-policy:"));
        assert!(headers.contains("connect-src 'none'"));
        assert!(headers.contains("cache-control: no-store"));
        let document = String::from_utf8(body).unwrap();
        assert!(document.contains("__HYPER_PREVIEW_PROBE__"));
        assert!(document.contains(&accepted.artifact_id.to_string()));
        assert!(!document.contains(ARTIFACT_BOOTSTRAP_MARKER));

        let source_map_path = format!(
            "/agent/artifact/{}/source-map?token={token}&session_id=6",
            accepted.artifact_id
        );
        let (status, source_map) =
            request_path(gateway.address(), &source_map_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert_eq!(source_map, b"{\"version\":3}");
        let source_path = format!(
            "/agent/artifact/{}/source?token={token}&session_id=6",
            accepted.artifact_id
        );
        let (status, headers, source) =
            request_path_raw(gateway.address(), &source_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-type: application/json"));
        assert!(headers.contains("cache-control: no-store"));
        let source: serde_json::Value = serde_json::from_slice(&source).unwrap();
        assert_eq!(source["artifact_id"], accepted.artifact_id.to_string());
        assert_eq!(source["source_revision"], 9);
        assert_eq!(source["entrypoint"], "/App.tsx");
        assert_eq!(
            source["files"]["/App.tsx"],
            "export default () => <main>ready</main>;"
        );
        let lsp_path = format!(
            "/agent/artifact/{}/lsp?token={token}&session_id=6",
            accepted.artifact_id
        );
        let lsp_request = serde_json::to_vec(&serde_json::json!({
            "source_revision": 9,
            "document_path": "/App.tsx",
            "source": "export default () => <main>ready</main>;",
            "kind": "diagnostics"
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &lsp_path, "POST", &lsp_request,)
                .await
                .0,
            StatusCode::FORBIDDEN.as_u16()
        );
        let unauthorized_source_path = format!(
            "/agent/artifact/{}/source?token=wrong&session_id=6",
            accepted.artifact_id
        );
        assert_eq!(
            request_path(gateway.address(), &unauthorized_source_path, "GET", b"")
                .await
                .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let next_operation = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "b".repeat(64),
                },
                "Compile second preview fixture".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let next_authorized = daemon
            .decide_permission(
                task_id,
                next_operation.operation_id,
                next_operation.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let next_dispatching = daemon
            .begin_operation(
                task_id,
                next_operation.operation_id,
                next_authorized.revision,
            )
            .unwrap();
        let next_bundle = "globalThis.__HYPER_PREVIEW_PROBE__ = 'second';";
        let next = daemon
            .accept_genui_artifact_from_base(
                task_id,
                next_operation.operation_id,
                next_dispatching.revision,
                accepted.artifact_id,
                accepted.source_revision,
                hyper_term_protocol::GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 10,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([(
                        "/App.tsx".into(),
                        "export default () => <main>second</main>;".into(),
                    )]),
                    bundle: next_bundle.into(),
                    css: String::new(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: sha256_bytes(next_bundle.as_bytes()),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        let history_path = format!(
            "/agent/artifact/{}/history?token={token}&session_id=6",
            next.artifact_id
        );
        let (status, history) = request_path(gateway.address(), &history_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let history: serde_json::Value = serde_json::from_slice(&history).unwrap();
        assert_eq!(history["active_artifact_id"], next.artifact_id.to_string());
        assert_eq!(history["entries"].as_array().unwrap().len(), 2);
        assert_eq!(
            history["entries"][0]["artifact"]["artifact_id"],
            next.artifact_id.to_string()
        );
        assert_eq!(
            history["entries"][1]["artifact"]["artifact_id"],
            accepted.artifact_id.to_string()
        );
        let historical_source_path = format!(
            "/agent/artifact/{}/history/{}/source?token={token}&session_id=6",
            next.artifact_id, accepted.artifact_id
        );
        let (status, historical_source) =
            request_path(gateway.address(), &historical_source_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let historical_source: serde_json::Value =
            serde_json::from_slice(&historical_source).unwrap();
        assert_eq!(historical_source["source_revision"], 9);
        assert_eq!(
            historical_source["files"]["/App.tsx"],
            "export default () => <main>ready</main>;"
        );
        let stale_history_path = format!(
            "/agent/artifact/{}/history?token={token}&session_id=6",
            accepted.artifact_id
        );
        assert_eq!(
            request_path(gateway.address(), &stale_history_path, "GET", b"")
                .await
                .0,
            StatusCode::NOT_FOUND.as_u16()
        );
        let stale_path = format!(
            "/agent/artifact/{}/preview?token={token}&session_id=6",
            ArtifactId::new()
        );
        assert_eq!(
            request_path(gateway.address(), &stale_path, "GET", b"")
                .await
                .0,
            StatusCode::NOT_FOUND.as_u16()
        );

        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proposal_only_agent_can_reject_an_effect_and_finish_the_turn() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"thread\":{\"id\":\"thread-4\"}}}' ;;\n    *'\"method\":\"turn/start\"'*)\n      printf '%s\\n' '{\"id\":3,\"result\":{\"turn\":{\"id\":\"turn-2\"}}}'\n      printf '%s\\n' '{\"id\":77,\"method\":\"item/commandExecution/requestApproval\",\"params\":{\"threadId\":\"thread-4\",\"turnId\":\"turn-2\",\"itemId\":\"command-1\",\"command\":\"touch forbidden\"}}' ;;\n    *'\"id\":77'*'\"decision\":\"decline\"'*)\n      printf '%s\\n' '{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread-4\",\"turnId\":\"turn-2\",\"itemId\":\"message-2\",\"delta\":\"The command was rejected.\"}}'\n      printf '%s\\n' '{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread-4\",\"turn\":{\"id\":\"turn-2\",\"status\":\"completed\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex)
            .expect("fake Codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).expect("fake Codex executable");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace,
            state_directory: state,
            daemon,
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        assert_eq!(
            request(gateway.address(), &token, 4, "POST").await.0,
            StatusCode::OK.as_u16()
        );
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/turn?token={token}&session_id=4"),
                "POST",
                b"Try a command",
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );

        let (operation_id, operation_revision) =
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let (status, body) = request(gateway.address(), &token, 4, "GET").await;
                    assert_eq!(status, StatusCode::OK.as_u16());
                    let snapshot: serde_json::Value =
                        serde_json::from_slice(&body).expect("snapshot response");
                    assert_ne!(
                        snapshot["status"], "failed",
                        "Agent failed before approval: {snapshot}"
                    );
                    if snapshot["status"] == "waiting_approval" {
                        let approval = snapshot["document"]["blocks"]
                            .as_array()
                            .expect("snapshot blocks")
                            .iter()
                            .find(|block| block["kind"] == "approval")
                            .expect("approval block");
                        break (
                            approval["payload"]["operation_id"]
                                .as_str()
                                .expect("operation id")
                                .to_owned(),
                            approval["payload"]["operation_revision"]
                                .as_u64()
                                .expect("operation revision"),
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Agent did not reach waiting approval");
        let unsafe_decision = serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "decision": "allow_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=4"),
            "POST",
            &serde_json::to_vec(&unsafe_decision).expect("unsafe permission decision"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN.as_u16());

        let decision = serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "decision": "reject_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=4"),
            "POST",
            &serde_json::to_vec(&decision).expect("permission decision"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());

        let snapshot = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (status, body) = request(gateway.address(), &token, 4, "GET").await;
                assert_eq!(status, StatusCode::OK.as_u16());
                let snapshot: serde_json::Value =
                    serde_json::from_slice(&body).expect("snapshot response");
                if snapshot["status"] == "completed" {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Agent did not complete after rejection");
        let blocks = snapshot["document"]["blocks"]
            .as_array()
            .expect("snapshot blocks");
        assert!(blocks.iter().any(|block| {
            block["kind"] == "operation" && block["payload"]["state"] == "cancelled"
        }));
        assert!(blocks.iter().any(|block| {
            block["kind"] == "approval"
                && block["payload"]["decision"] == "reject_once"
                && block["actions"].as_array().is_some_and(Vec::is_empty)
        }));
        assert!(blocks.iter().any(|block| {
            block["payload"]["role"] == "agent"
                && block["payload"]["text"] == "The command was rejected."
        }));

        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_known_read_only_mcp_operations_can_be_allowed_from_agent_chrome() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"thread\":{\"id\":\"thread-mcp\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex)
            .expect("fake Codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).expect("fake Codex executable");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace,
            state_directory: state,
            daemon: daemon.clone(),
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        let (status, body) = request(gateway.address(), &token, 5, "POST").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).expect("start response");
        let task_id = TaskId::from_uuid(
            uuid::Uuid::parse_str(response["task_id"].as_str().expect("task id"))
                .expect("task UUID"),
        );
        let mcp = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.diff.review".into(),
                    payload_digest: "a".repeat(64),
                },
                "Build a bounded diff review".into(),
                RiskClass::ReadOnly,
                vec!["diff_review".into()],
            )
            .expect("MCP proposal");
        let allow = serde_json::json!({
            "operation_id": mcp.operation_id,
            "expected_revision": mcp.revision,
            "decision": "allow_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=5"),
            "POST",
            &serde_json::to_vec(&allow).expect("allow decision"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        assert_eq!(
            daemon
                .operation(mcp.operation_id)
                .expect("MCP operation")
                .state,
            hyper_term_protocol::OperationState::Authorized
        );

        let opaque = daemon
            .propose_operation(
                task_id,
                OperationKind::Other("agent_shell".into()),
                OperationAction::Opaque {
                    kind: "item/commandExecution/requestApproval".into(),
                    payload_digest: "b".repeat(64),
                },
                "touch forbidden".into(),
                RiskClass::ExternalEffect,
                vec!["shell".into()],
            )
            .expect("opaque proposal");
        let unsafe_allow = serde_json::json!({
            "operation_id": opaque.operation_id,
            "expected_revision": opaque.revision,
            "decision": "allow_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=5"),
            "POST",
            &serde_json::to_vec(&unsafe_allow).expect("unsafe allow decision"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN.as_u16());

        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[test]
    fn brokered_mcp_receives_pinned_deno_tools_and_a_private_workspace_snapshot() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state_directory = temporary.path().join("gateway-state");
        std::fs::create_dir_all(workspace.join("src")).expect("workspace");
        std::fs::create_dir_all(workspace.join("node_modules/ignored")).expect("dependencies");
        std::fs::write(
            workspace.join("src/main.ts"),
            "export const answer: number = 42;\n",
        )
        .expect("source");
        std::fs::write(
            workspace.join("node_modules/ignored/index.ts"),
            "export const ignored = true;\n",
        )
        .expect("generated dependency");
        std::fs::create_dir_all(&state_directory).expect("state directory");
        let mcp = temporary.path().join("hyper-term-mcp");
        let deno = temporary.path().join("deno");
        let script = temporary.path().join("genui-compiler.js");
        let wasm = temporary.path().join("esbuild.wasm");
        let preview = temporary.path().join("genui-preview.html");
        std::fs::write(&mcp, "mcp").expect("mcp");
        std::fs::write(&deno, "deno").expect("deno");
        std::fs::write(&script, "compiler").expect("compiler");
        std::fs::write(&wasm, "wasm").expect("wasm");
        std::fs::write(
            &preview,
            "<!-- HYPER_TERM_ARTIFACT_BOOTSTRAP -->hyper_term_preview_boot",
        )
        .expect("preview");
        let runtime = AgentGatewayRuntime {
            config: Arc::new(AgentGatewayConfig {
                bind: "127.0.0.1:0".parse().expect("bind"),
                token: "0123456789abcdef0123456789abcdef".into(),
                workspace: workspace.canonicalize().unwrap(),
                state_directory: state_directory.canonicalize().unwrap(),
                daemon: DaemonState::open(temporary.path().join("daemon-state")).unwrap(),
                codex_executable: None,
                codex_auth_file: None,
                acp_providers: Vec::new(),
                mcp_executable: Some(mcp),
                genui_runtime: Some(AgentGenUiRuntimeConfig {
                    deno_executable: deno,
                    runtime_version: "2.9.3".into(),
                    compiler_script: script,
                    compiler_wasm: wasm,
                    preview_shell: preview,
                    compiler_version: "0.28.1".into(),
                }),
                workbench_assets: None,
                debug_capsule: None,
                control_socket: temporary.path().join("hyperd.sock"),
            }),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            preview_shell: None,
            workbench_assets: None,
            editor_lsp: None,
            artifact_draft_compiler: None,
            artifact_editor_store: Arc::new(ArtifactEditorStore::open(&state_directory).unwrap()),
            artifact_editor_lock: Arc::new(Mutex::new(())),
            artifact_runtime_trace_store: Arc::new(
                ArtifactRuntimeTraceStore::open(&state_directory).unwrap(),
            ),
            artifact_runtime_trace_lock: Arc::new(Mutex::new(())),
            artifact_drafts: Arc::new(Mutex::new(HashMap::new())),
            workspace_applies: Arc::new(Mutex::new(HashMap::new())),
            workspace_recovery_block: Arc::new(Mutex::new(None)),
        };
        let session_root = state_directory.join("agents/session-7");
        let config = runtime
            .mcp_launch(TaskId::new(), &session_root)
            .expect("MCP configured")
            .expect("valid MCP config");
        let arguments = config
            .arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(arguments.contains(&std::borrow::Cow::Borrowed("--genui-script")));
        assert!(arguments.contains(&std::borrow::Cow::Borrowed("--genui-wasm")));
        assert!(arguments.contains(&std::borrow::Cow::Borrowed("--deno-sha256")));
        let snapshot = arguments
            .windows(2)
            .find(|pair| pair[0] == "--workspace-snapshot")
            .map(|pair| PathBuf::from(pair[1].as_ref()))
            .expect("workspace snapshot argument");
        assert_eq!(
            std::fs::read_to_string(snapshot.join("src/main.ts")).unwrap(),
            "export const answer: number = 42;\n"
        );
        assert!(!snapshot.join("node_modules").exists());
        assert!(arguments.iter().any(|argument| argument.len() == 64));
        assert!(config.arguments.len() <= 32);

        std::fs::write(
            workspace.join("oversized.ts"),
            vec![b'x'; 2 * 1024 * 1024 + 1],
        )
        .expect("oversized source fixture");
        let degraded = runtime
            .mcp_launch(TaskId::new(), &state_directory.join("agents/session-8"))
            .expect("MCP configured")
            .expect("GenUI-only MCP config");
        let degraded_arguments = degraded
            .arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(degraded_arguments.contains(&std::borrow::Cow::Borrowed("--genui-script")));
        assert!(!degraded_arguments.contains(&std::borrow::Cow::Borrowed("--workspace-snapshot")));
        assert!(
            !state_directory
                .join("agents/session-8/deno-tools/workspace-snapshot")
                .exists()
        );
    }

    #[test]
    fn agent_session_capacity_is_reported_as_rate_limited() {
        assert_eq!(
            agent_start_error_response(StartError::Capacity).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    async fn request(
        address: SocketAddr,
        token: &str,
        session_id: u16,
        method: &str,
    ) -> (u16, Vec<u8>) {
        request_path(
            address,
            &format!("/agent/session?token={token}&session_id={session_id}"),
            method,
            b"",
        )
        .await
    }

    async fn request_path(
        address: SocketAddr,
        path: &str,
        method: &str,
        body: &[u8],
    ) -> (u16, Vec<u8>) {
        let (status, _, body) = request_path_raw(address, path, method, body).await;
        (status, body)
    }

    async fn request_path_raw(
        address: SocketAddr,
        path: &str,
        method: &str,
        body: &[u8],
    ) -> (u16, Vec<u8>, Vec<u8>) {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect agent gateway");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write agent request");
        stream
            .write_all(body)
            .await
            .expect("write agent request body");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read agent response");
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("HTTP response headers");
        let status = String::from_utf8_lossy(&response[..header_end])
            .split_whitespace()
            .nth(1)
            .expect("HTTP status")
            .parse()
            .expect("numeric HTTP status");
        (
            status,
            response[..header_end].to_vec(),
            response[header_end..].to_vec(),
        )
    }
}
