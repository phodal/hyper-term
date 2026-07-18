use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
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
    AgentDriverEvent, AgentEffectAuthorization, AgentEffectKind, CodexAppServerClient,
    CodexAppServerConfig, CodexMcpServerConfig, DriverState, ExternalRequestId, sha256_file,
};
use hyper_term_protocol::{
    ArtifactId, BlockDocument, BlockId, MessageRole, OperationAction, OperationId, OperationKind,
    PermissionDecision, RiskClass, TaskId,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::DaemonState;

const MIN_TOKEN_BYTES: usize = 32;
const MAX_AGENT_SESSIONS: usize = 8;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const START_TURN_TIMEOUT: Duration = Duration::from_secs(10);
const COMPLETE_TURN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_PROMPT_BYTES: usize = 16 * 1024;
const MAX_AGENT_MESSAGE_BYTES: usize = 256 * 1024;
const AGENT_CSP: &str = "default-src 'none'; frame-ancestors 'none'";
const PREVIEW_CSP: &str = "default-src 'none'; script-src 'unsafe-inline' blob:; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; font-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const MAX_PREVIEW_SHELL_BYTES: u64 = 4 * 1024 * 1024;
const ARTIFACT_BOOTSTRAP_MARKER: &str = "<!-- HYPER_TERM_ARTIFACT_BOOTSTRAP -->";

#[derive(Clone)]
pub struct AgentGatewayConfig {
    pub bind: SocketAddr,
    pub token: String,
    pub workspace: PathBuf,
    pub state_directory: PathBuf,
    pub daemon: DaemonState,
    pub codex_executable: Option<PathBuf>,
    pub codex_auth_file: Option<PathBuf>,
    pub mcp_executable: Option<PathBuf>,
    pub genui_runtime: Option<AgentGenUiRuntimeConfig>,
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
}

struct AgentSession {
    client: Arc<CodexAppServerClient>,
    task_id: TaskId,
    thread_id: String,
    progress: Mutex<AgentProgress>,
    pending_effect: Mutex<Option<PendingAgentEffect>>,
}

#[derive(Clone)]
struct AgentTurnProjection {
    turn_id: String,
    agent_block_id: BlockId,
    plan_block_id: BlockId,
    agent_message_bytes: usize,
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
}

#[derive(Serialize)]
struct AgentSessionResponse {
    session_id: u16,
    provider: &'static str,
    protocol: &'static str,
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
    document: BlockDocument,
}

#[derive(Serialize)]
struct AgentTurnResponse {
    session_id: u16,
    status: AgentStatus,
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

    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let runtime = AgentGatewayRuntime {
        config: Arc::new(config),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        preview_shell,
    };
    let router = Router::new()
        .route(
            "/agent/session",
            get(snapshot_session)
                .post(start_session)
                .delete(close_session),
        )
        .route("/agent/session/turn", post(start_turn))
        .route("/agent/session/permission", post(decide_permission))
        .route(
            "/agent/artifact/{artifact_id}/preview",
            get(preview_artifact),
        )
        .route(
            "/agent/artifact/{artifact_id}/source-map",
            get(artifact_source_map),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_PROMPT_BYTES))
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

async fn start_session(
    State(runtime): State<AgentGatewayRuntime>,
    Query(query): Query<AgentSessionQuery>,
) -> Response {
    let session_id = match authorize(&runtime, &query) {
        Ok(session_id) => session_id,
        Err(response) => return *response,
    };
    let result = tokio::task::spawn_blocking(move || runtime.start_codex(session_id)).await;
    match result {
        Ok(Ok(response)) => json_response(StatusCode::OK, &response),
        Ok(Err(StartError::Unavailable)) => status_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Codex app-server is unavailable",
        ),
        Ok(Err(_)) | Err(_) => status_response(
            StatusCode::BAD_GATEWAY,
            "Codex app-server failed to initialize",
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

fn authorize(
    runtime: &AgentGatewayRuntime,
    query: &AgentSessionQuery,
) -> Result<u16, Box<Response>> {
    if !constant_time_eq(
        query.token.as_deref().unwrap_or_default().as_bytes(),
        runtime.config.token.as_bytes(),
    ) {
        return Err(Box::new(status_response(
            StatusCode::UNAUTHORIZED,
            "agent gateway token is invalid",
        )));
    }
    let Some(session_id @ 1..=999) = query.session_id else {
        return Err(Box::new(status_response(
            StatusCode::BAD_REQUEST,
            "agent session id is invalid",
        )));
    };
    Ok(session_id)
}

#[derive(Debug)]
enum StartError {
    Unavailable,
    Capacity,
    Lock,
    Driver,
}

#[derive(Debug)]
enum SessionError {
    NotFound,
    Busy,
    PromptTooLarge,
    Lock,
    Daemon,
    Thread,
    NoPendingEffect,
    StalePermission,
    UnsafeApproval,
    Driver,
    ArtifactUnavailable,
}

impl AgentGatewayRuntime {
    fn start_codex(&self, session_id: u16) -> Result<AgentSessionResponse, StartError> {
        let mut sessions = self.sessions.lock().map_err(|_| StartError::Lock)?;
        if let Some(session) = sessions.get(&session_id) {
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
        let executable = self
            .config
            .codex_executable
            .as_ref()
            .ok_or(StartError::Unavailable)?
            .canonicalize()
            .map_err(|_| StartError::Unavailable)?;
        let executable_sha256 = sha256_file(&executable).map_err(|_| StartError::Driver)?;
        let session_root = self
            .config
            .state_directory
            .join("agents")
            .join(format!("session-{session_id}"));
        let task_id = self
            .config
            .daemon
            .create_task(format!("Codex Agent session {session_id}"))
            .map_err(|_| StartError::Driver)?;
        let brokered_mcp_server = self.mcp_config(task_id, &session_root).transpose()?;
        let client = CodexAppServerClient::launch(CodexAppServerConfig {
            executable,
            executable_sha256,
            implementation_version: "installed".into(),
            workspace: self.config.workspace.clone(),
            codex_home: session_root.join("codex-home"),
            scratch_directory: session_root.join("scratch"),
            auth_file: self.config.codex_auth_file.clone(),
            brokered_mcp_server,
        })
        .map_err(|_| StartError::Driver)?;
        client
            .initialize(INITIALIZE_TIMEOUT)
            .map_err(|_| StartError::Driver)?;
        let thread_id = client
            .start_thread(INITIALIZE_TIMEOUT)
            .map_err(|_| StartError::Driver)?;
        let session = Arc::new(AgentSession {
            client: Arc::new(client),
            task_id,
            thread_id,
            progress: Mutex::new(AgentProgress {
                status: AgentStatus::Ready,
                turn_id: None,
                error: None,
            }),
            pending_effect: Mutex::new(None),
        });
        let response = ready_response(session_id, &session);
        sessions.insert(session_id, session);
        Ok(response)
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
        Ok(AgentSnapshotResponse {
            session_id,
            status,
            turn_id,
            error,
            document,
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
            if request.decision == PermissionDecision::AllowOnce
                && !allowable_brokered_mcp_operation(&operation)
            {
                return Err(SessionError::UnsafeApproval);
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
            set_progress_failed(
                &session,
                "Rejected Agent effect could not be returned to Codex",
            );
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

    fn mcp_config(
        &self,
        task_id: TaskId,
        session_root: &std::path::Path,
    ) -> Option<Result<CodexMcpServerConfig, StartError>> {
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
            let deno_root = session_root.join("deno-genui");
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
        Some(Ok(CodexMcpServerConfig {
            executable,
            executable_sha256: digest,
            arguments,
        }))
    }

    fn close_session(&self, session_id: u16) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.remove(&session_id)
        {
            let _ = session.client.close();
        }
    }

    fn close_all(&self) {
        let sessions = if let Ok(mut sessions) = self.sessions.lock() {
            sessions.drain().map(|(_, session)| session).collect()
        } else {
            Vec::new()
        };
        for session in sessions {
            let _ = session.client.close();
        }
    }
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
            plan_block_id: BlockId::new(),
            agent_message_bytes: 0,
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
                        Some(projection.turn_id.clone()),
                        text,
                    )
                    .is_err()
                {
                    set_progress_failed(&session, "Agent response could not be journaled");
                    let _ = session.client.close();
                    return;
                }
            }
            AgentDriverEvent::PlanDelta {
                thread_id,
                turn_id: event_turn_id,
                text,
                ..
            } if thread_id == session.thread_id && event_turn_id == projection.turn_id => {
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
                    set_progress_failed(&session, "Codex reported a failed turn");
                } else if let Ok(mut progress) = session.progress.lock() {
                    progress.status = AgentStatus::Completed;
                    progress.error = None;
                }
                return;
            }
            AgentDriverEvent::Exited { .. } => {
                set_progress_failed(&session, "Codex exited before the turn completed");
                return;
            }
            _ => {}
        }
    }
}

fn operation_kind_and_risk(kind: AgentEffectKind) -> (OperationKind, RiskClass) {
    match kind {
        AgentEffectKind::Shell => (
            OperationKind::Other("codex_shell".into()),
            RiskClass::ExternalEffect,
        ),
        AgentEffectKind::WorkspaceEdit => (OperationKind::FileEdit, RiskClass::WorkspaceWrite),
        AgentEffectKind::Tool => (OperationKind::AgentTool, RiskClass::ExternalEffect),
        AgentEffectKind::ComputerUse => (OperationKind::ComputerUse, RiskClass::ExternalEffect),
        AgentEffectKind::Opaque => (
            OperationKind::Other("codex_effect".into()),
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
        provider: "codex",
        protocol: "codex-app-server-v2",
        status: "ready",
        task_id: session.task_id,
        thread_id: session.thread_id.clone(),
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
    use std::os::unix::fs::PermissionsExt;

    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

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
            mcp_executable: None,
            genui_runtime: None,
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

        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session/turn?token={token}&session_id=3"),
            "POST",
            b"Reply with the live marker",
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");

        let snapshot = loop {
            let (status, body) = request(gateway.address(), &token, 3, "GET").await;
            assert_eq!(status, StatusCode::OK.as_u16());
            let snapshot: serde_json::Value =
                serde_json::from_slice(&body).expect("snapshot response");
            if snapshot["status"] == "completed" {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
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

        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: gateway_state,
            daemon: daemon.clone(),
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            mcp_executable: None,
            genui_runtime: Some(AgentGenUiRuntimeConfig {
                deno_executable: deno,
                runtime_version: "2.9.3".into(),
                compiler_script,
                compiler_wasm,
                preview_shell,
                compiler_version: "0.28.1".into(),
            }),
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        let (status, body) = request(gateway.address(), &token, 6, "POST").await;
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
            mcp_executable: None,
            genui_runtime: None,
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
            mcp_executable: None,
            genui_runtime: None,
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
                OperationKind::Other("codex_shell".into()),
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
    fn brokered_mcp_receives_pinned_genui_runtime_arguments() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state_directory = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
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
                mcp_executable: Some(mcp),
                genui_runtime: Some(AgentGenUiRuntimeConfig {
                    deno_executable: deno,
                    runtime_version: "2.9.3".into(),
                    compiler_script: script,
                    compiler_wasm: wasm,
                    preview_shell: preview,
                    compiler_version: "0.28.1".into(),
                }),
                control_socket: temporary.path().join("hyperd.sock"),
            }),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            preview_shell: None,
        };
        let session_root = state_directory.join("agents/session-7");
        let config = runtime
            .mcp_config(TaskId::new(), &session_root)
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
        assert!(arguments.iter().any(|argument| argument.len() == 64));
        assert!(config.arguments.len() <= 32);
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
