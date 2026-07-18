use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::post;
use hyper_term_drivers::{
    CodexAppServerClient, CodexAppServerConfig, CodexMcpServerConfig, DriverState, sha256_file,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const MIN_TOKEN_BYTES: usize = 32;
const MAX_AGENT_SESSIONS: usize = 8;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const AGENT_CSP: &str = "default-src 'none'; frame-ancestors 'none'";

#[derive(Clone)]
pub struct AgentGatewayConfig {
    pub bind: SocketAddr,
    pub token: String,
    pub workspace: PathBuf,
    pub state_directory: PathBuf,
    pub codex_executable: Option<PathBuf>,
    pub mcp_executable: Option<PathBuf>,
    pub control_socket: PathBuf,
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
    #[error("agent gateway I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent gateway task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
struct AgentGatewayRuntime {
    config: Arc<AgentGatewayConfig>,
    sessions: Arc<Mutex<HashMap<u16, Arc<CodexAppServerClient>>>>,
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

    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let runtime = AgentGatewayRuntime {
        config: Arc::new(config),
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let router = Router::new()
        .route("/agent/session", post(start_session).delete(close_session))
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

impl AgentGatewayRuntime {
    fn start_codex(&self, session_id: u16) -> Result<AgentSessionResponse, StartError> {
        let mut sessions = self.sessions.lock().map_err(|_| StartError::Lock)?;
        if let Some(session) = sessions.get(&session_id) {
            return match session.state().map_err(|_| StartError::Driver)? {
                DriverState::Ready | DriverState::Busy | DriverState::Waiting => {
                    Ok(ready_response(session_id))
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
        let brokered_mcp_server = self.mcp_config().transpose()?;
        let client = CodexAppServerClient::launch(CodexAppServerConfig {
            executable,
            executable_sha256,
            implementation_version: "installed".into(),
            workspace: self.config.workspace.clone(),
            codex_home: session_root.join("codex-home"),
            scratch_directory: session_root.join("scratch"),
            brokered_mcp_server,
        })
        .map_err(|_| StartError::Driver)?;
        client
            .initialize(INITIALIZE_TIMEOUT)
            .map_err(|_| StartError::Driver)?;
        sessions.insert(session_id, Arc::new(client));
        Ok(ready_response(session_id))
    }

    fn mcp_config(&self) -> Option<Result<CodexMcpServerConfig, StartError>> {
        let executable = match self.config.mcp_executable.as_ref()?.canonicalize() {
            Ok(executable) => executable,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let digest = match sha256_file(&executable) {
            Ok(digest) => digest,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        Some(Ok(CodexMcpServerConfig {
            executable,
            executable_sha256: digest,
            arguments: vec![
                "--agent-mode".into(),
                "--socket".into(),
                self.config.control_socket.clone().into_os_string(),
            ],
        }))
    }

    fn close_session(&self, session_id: u16) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.remove(&session_id)
        {
            let _ = session.close();
        }
    }

    fn close_all(&self) {
        let sessions = if let Ok(mut sessions) = self.sessions.lock() {
            sessions.drain().map(|(_, session)| session).collect()
        } else {
            Vec::new()
        };
        for session in sessions {
            let _ = session.close();
        }
    }
}

fn ready_response(session_id: u16) -> AgentSessionResponse {
    AgentSessionResponse {
        session_id,
        provider: "codex",
        protocol: "codex-app-server-v2",
        status: "ready",
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
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(AGENT_CSP));
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

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_start_initializes_and_closes_a_pinned_codex_adapter() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n  esac\ndone\n",
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
            codex_executable: Some(fake_codex),
            mcp_executable: None,
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

        assert_eq!(
            request(gateway.address(), &token, 3, "DELETE").await.0,
            StatusCode::NO_CONTENT.as_u16()
        );
        gateway.shutdown().await.expect("shutdown gateway");
    }

    async fn request(
        address: SocketAddr,
        token: &str,
        session_id: u16,
        method: &str,
    ) -> (u16, Vec<u8>) {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect agent gateway");
        let request = format!(
            "{method} /agent/session?token={token}&session_id={session_id} HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write agent request");
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
        (status, response[header_end..].to_vec())
    }
}
