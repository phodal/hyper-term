use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ORIGIN, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use hyper_term_core::{TerminalEvent, TerminalReplay, TerminalSubscription};
use hyper_term_protocol::{
    ClientId, InputLeaseId, TERMINAL_WEB_PROTOCOL_VERSION, TerminalAttachmentId, TerminalId,
    TerminalInputFrame, TerminalSize, TerminalWebBinaryFrame, TerminalWebClientControl,
    TerminalWebServerControl, decode_terminal_web_binary, encode_terminal_web_binary,
};
use serde::Deserialize;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::{DaemonError, DaemonState};

const MIN_TOKEN_BYTES: usize = 32;
const OUTBOUND_QUEUE_MESSAGES: usize = 256;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIPTION_POLL: Duration = Duration::from_millis(50);
const TERMINAL_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://127.0.0.1:* ws://[::1]:*; img-src 'self' data:; font-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

#[derive(Clone, Debug)]
pub struct TerminalGatewayConfig {
    pub bind: SocketAddr,
    pub assets: PathBuf,
    pub token: String,
    /// Authority-owned starting directory used when a trusted renderer does
    /// not request an explicit directory for a new human shell.
    pub default_cwd: Option<PathBuf>,
}

#[derive(Debug)]
pub struct TerminalGatewayHandle {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), std::io::Error>>>,
}

impl TerminalGatewayHandle {
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub async fn shutdown(mut self) -> Result<(), TerminalGatewayError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}

impl Drop for TerminalGatewayHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[derive(Debug, Error)]
pub enum TerminalGatewayError {
    #[error("terminal gateway must bind to a loopback address, got {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("terminal gateway token must contain at least {MIN_TOKEN_BYTES} bytes")]
    WeakToken,
    #[error("terminal gateway asset directory is invalid: {0}")]
    InvalidAssets(PathBuf),
    #[error("terminal gateway I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("terminal gateway task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
struct GatewayRuntime {
    daemon: DaemonState,
    assets: Arc<PathBuf>,
    token: Arc<str>,
    origin: Arc<str>,
    default_cwd: Option<PathBuf>,
    attachments: Arc<Mutex<HashMap<TerminalAttachmentId, Attachment>>>,
}

#[derive(Clone, Copy)]
struct Attachment {
    terminal_id: TerminalId,
    client_id: ClientId,
    next_input_sequence: u64,
    resize_generation: u64,
    active_connection: Option<Uuid>,
}

struct ActiveAttachment {
    attachment_id: TerminalAttachmentId,
    connection_id: Uuid,
    terminal_id: TerminalId,
    client_id: ClientId,
    lease_id: InputLeaseId,
    next_input_sequence: u64,
    resize_generation: u64,
}

#[derive(Debug, Error)]
enum ConnectionError {
    #[error("the first terminal message must be a hello control message")]
    HelloRequired,
    #[error("terminal protocol version {0} is unsupported")]
    UnsupportedProtocol(u16),
    #[error("terminal attachment is already connected")]
    AttachmentBusy,
    #[error("terminal input sequence {actual} does not match expected {expected}")]
    InputSequence { expected: u64, actual: u64 },
    #[error("terminal resize generation {actual} does not match expected {expected}")]
    ResizeGeneration { expected: u64, actual: u64 },
    #[error("terminal WebSocket message is invalid: {0}")]
    InvalidMessage(String),
    #[error("terminal attachment state is unavailable")]
    AttachmentState,
    #[error(transparent)]
    Daemon(#[from] DaemonError),
}

impl ConnectionError {
    fn code(&self) -> &'static str {
        match self {
            Self::HelloRequired => "hello_required",
            Self::UnsupportedProtocol(_) => "protocol",
            Self::AttachmentBusy => "attachment_busy",
            Self::InputSequence { .. } => "input_sequence",
            Self::ResizeGeneration { .. } => "resize_generation",
            Self::InvalidMessage(_) => "invalid_message",
            Self::AttachmentState => "attachment_state",
            Self::Daemon(_) => "daemon",
        }
    }

    fn control(&self) -> TerminalWebServerControl {
        TerminalWebServerControl::Error {
            code: self.code().into(),
            message: self.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct AuthQuery {
    token: Option<String>,
}

enum Outbound {
    Binary(Vec<u8>),
    Control(TerminalWebServerControl),
    End,
}

pub async fn spawn_terminal_gateway(
    config: TerminalGatewayConfig,
    daemon: DaemonState,
) -> Result<TerminalGatewayHandle, TerminalGatewayError> {
    if !config.bind.ip().is_loopback() {
        return Err(TerminalGatewayError::NonLoopbackBind(config.bind));
    }
    if config.token.len() < MIN_TOKEN_BYTES {
        return Err(TerminalGatewayError::WeakToken);
    }
    let assets = config
        .assets
        .canonicalize()
        .map_err(|_| TerminalGatewayError::InvalidAssets(config.assets.clone()))?;
    if !assets.is_dir() || !assets.join("index.html").is_file() {
        return Err(TerminalGatewayError::InvalidAssets(assets));
    }

    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let runtime = GatewayRuntime {
        daemon,
        assets: Arc::new(assets),
        token: Arc::from(config.token),
        origin: Arc::from(format!("http://{address}")),
        default_cwd: config.default_cwd,
        attachments: Arc::new(Mutex::new(HashMap::new())),
    };
    let router = Router::new()
        .route("/terminal", get(upgrade_terminal))
        .fallback(get(serve_asset))
        .with_state(runtime);
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_receiver.await;
            })
            .await
    });
    Ok(TerminalGatewayHandle {
        address,
        shutdown: Some(shutdown_sender),
        task: Some(task),
    })
}

async fn upgrade_terminal(
    State(runtime): State<GatewayRuntime>,
    Query(auth): Query<AuthQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !constant_time_eq(
        auth.token.as_deref().unwrap_or_default().as_bytes(),
        runtime.token.as_bytes(),
    ) {
        return status_response(
            StatusCode::UNAUTHORIZED,
            "terminal gateway token is invalid",
        );
    }
    if headers.get(ORIGIN).and_then(|value| value.to_str().ok()) != Some(runtime.origin.as_ref()) {
        return status_response(StatusCode::FORBIDDEN, "terminal gateway origin is invalid");
    }

    upgrade
        .max_message_size(hyper_term_protocol::MAX_TERMINAL_WEB_PAYLOAD_BYTES + 1024)
        .on_upgrade(move |socket| terminal_connection(socket, runtime))
        .into_response()
}

async fn terminal_connection(mut socket: WebSocket, runtime: GatewayRuntime) {
    let hello = match receive_hello(&mut socket).await {
        Ok(hello) => hello,
        Err(error) => {
            let _ = send_control(&mut socket, error.control()).await;
            let _ = socket.close().await;
            return;
        }
    };
    let after_sequence = match &hello {
        TerminalWebClientControl::Hello { after_sequence, .. } => *after_sequence,
        _ => unreachable!("receive_hello returns only hello"),
    };
    let active = match runtime.activate(hello) {
        Ok(active) => active,
        Err(error) => {
            let _ = send_control(&mut socket, error.control()).await;
            let _ = socket.close().await;
            return;
        }
    };
    let subscription = match runtime
        .daemon
        .terminal_subscription(active.terminal_id, after_sequence)
    {
        Ok(subscription) => subscription,
        Err(error) => {
            let error = ConnectionError::Daemon(error);
            let _ = send_control(&mut socket, error.control()).await;
            runtime.deactivate(&active);
            let _ = socket.close().await;
            return;
        }
    };

    let ready = TerminalWebServerControl::Ready {
        protocol_version: TERMINAL_WEB_PROTOCOL_VERSION,
        attachment_id: active.attachment_id,
        terminal_id: active.terminal_id,
        next_input_sequence: active.next_input_sequence,
        resize_generation: active.resize_generation,
    };
    if send_control(&mut socket, ready).await.is_err()
        || send_replay(&mut socket, &subscription).await.is_err()
    {
        runtime.deactivate(&active);
        return;
    }
    if let Some(exit) = subscription.exit.clone() {
        let _ = send_control(
            &mut socket,
            TerminalWebServerControl::Exited {
                exit_code: exit.exit_code,
                signal: exit.signal,
            },
        )
        .await;
        runtime.deactivate(&active);
        let _ = socket.close().await;
        return;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let (outbound_sender, outbound_receiver) = mpsc::channel(OUTBOUND_QUEUE_MESSAGES);
    let output_task = spawn_output_pump(subscription, outbound_sender.clone(), Arc::clone(&cancel));
    let (socket_sender, socket_receiver) = socket.split();
    tokio::select! {
        _ = write_socket(socket_sender, outbound_receiver) => {}
        _ = read_socket(socket_receiver, outbound_sender, runtime.clone(), &active) => {}
    }
    cancel.store(true, Ordering::Release);
    let _ = output_task.await;
    runtime.deactivate(&active);
}

async fn receive_hello(
    socket: &mut WebSocket,
) -> Result<TerminalWebClientControl, ConnectionError> {
    let message = tokio::time::timeout(HELLO_TIMEOUT, socket.recv())
        .await
        .map_err(|_| ConnectionError::HelloRequired)?
        .ok_or(ConnectionError::HelloRequired)?
        .map_err(|error| ConnectionError::InvalidMessage(error.to_string()))?;
    let Message::Text(text) = message else {
        return Err(ConnectionError::HelloRequired);
    };
    let control = serde_json::from_str::<TerminalWebClientControl>(&text)
        .map_err(|error| ConnectionError::InvalidMessage(error.to_string()))?;
    let TerminalWebClientControl::Hello {
        protocol_version, ..
    } = control
    else {
        return Err(ConnectionError::HelloRequired);
    };
    if protocol_version != TERMINAL_WEB_PROTOCOL_VERSION {
        return Err(ConnectionError::UnsupportedProtocol(protocol_version));
    }
    Ok(control)
}

impl GatewayRuntime {
    fn activate(
        &self,
        hello: TerminalWebClientControl,
    ) -> Result<ActiveAttachment, ConnectionError> {
        let TerminalWebClientControl::Hello {
            attachment_id,
            size,
            cwd,
            ..
        } = hello
        else {
            return Err(ConnectionError::HelloRequired);
        };
        size.validate()
            .map_err(|message| ConnectionError::InvalidMessage(message.into()))?;
        let connection_id = Uuid::new_v4();
        let mut attachments = lock(&self.attachments)?;
        if let Some(attachment_id) = attachment_id
            && let Some(attachment) = attachments.get_mut(&attachment_id)
        {
            if attachment.active_connection.is_some() {
                return Err(ConnectionError::AttachmentBusy);
            }
            let next_resize_generation = attachment.resize_generation + 1;
            self.daemon
                .resize_terminal(attachment.terminal_id, next_resize_generation, size)?;
            let (lease_id, _) = self
                .daemon
                .acquire_input_lease(attachment.terminal_id, attachment.client_id)?;
            attachment.resize_generation = next_resize_generation;
            attachment.active_connection = Some(connection_id);
            return Ok(ActiveAttachment {
                attachment_id,
                connection_id,
                terminal_id: attachment.terminal_id,
                client_id: attachment.client_id,
                lease_id,
                next_input_sequence: attachment.next_input_sequence,
                resize_generation: attachment.resize_generation,
            });
        }

        let attachment_id = TerminalAttachmentId::new();
        let terminal_id = self
            .daemon
            .open_user_shell(cwd.or_else(|| self.default_cwd.clone()), size)?;
        let client_id = ClientId::new();
        let (lease_id, _) = match self.daemon.acquire_input_lease(terminal_id, client_id) {
            Ok(lease) => lease,
            Err(error) => {
                let _ = self.daemon.close_terminal(terminal_id);
                return Err(error.into());
            }
        };
        let attachment = Attachment {
            terminal_id,
            client_id,
            next_input_sequence: 1,
            resize_generation: 0,
            active_connection: Some(connection_id),
        };
        attachments.insert(attachment_id, attachment);
        Ok(ActiveAttachment {
            attachment_id,
            connection_id,
            terminal_id,
            client_id,
            lease_id,
            next_input_sequence: attachment.next_input_sequence,
            resize_generation: attachment.resize_generation,
        })
    }

    fn accept_input(
        &self,
        active: &ActiveAttachment,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> Result<(), ConnectionError> {
        let mut attachments = lock(&self.attachments)?;
        let attachment = connected_attachment(&mut attachments, active)?;
        if sequence != attachment.next_input_sequence {
            return Err(ConnectionError::InputSequence {
                expected: attachment.next_input_sequence,
                actual: sequence,
            });
        }
        self.daemon.write_terminal_input(
            active.client_id,
            TerminalInputFrame {
                terminal_id: active.terminal_id,
                lease_id: active.lease_id,
                sequence,
                bytes,
            },
        )?;
        attachment.next_input_sequence += 1;
        Ok(())
    }

    fn accept_resize(
        &self,
        active: &ActiveAttachment,
        generation: u64,
        size: TerminalSize,
    ) -> Result<(), ConnectionError> {
        let mut attachments = lock(&self.attachments)?;
        let attachment = connected_attachment(&mut attachments, active)?;
        let expected = attachment.resize_generation + 1;
        if generation != expected {
            return Err(ConnectionError::ResizeGeneration {
                expected,
                actual: generation,
            });
        }
        self.daemon
            .resize_terminal(active.terminal_id, generation, size)?;
        attachment.resize_generation = generation;
        Ok(())
    }

    fn close_attachment(&self, active: &ActiveAttachment) -> Result<(), ConnectionError> {
        self.daemon.close_terminal(active.terminal_id)?;
        lock(&self.attachments)?.remove(&active.attachment_id);
        Ok(())
    }

    fn deactivate(&self, active: &ActiveAttachment) {
        let _ =
            self.daemon
                .release_input_lease(active.terminal_id, active.lease_id, active.client_id);
        if let Ok(mut attachments) = self.attachments.lock()
            && let Some(attachment) = attachments.get_mut(&active.attachment_id)
            && attachment.active_connection == Some(active.connection_id)
        {
            attachment.active_connection = None;
        }
    }
}

fn connected_attachment<'a>(
    attachments: &'a mut HashMap<TerminalAttachmentId, Attachment>,
    active: &ActiveAttachment,
) -> Result<&'a mut Attachment, ConnectionError> {
    let attachment = attachments
        .get_mut(&active.attachment_id)
        .ok_or(ConnectionError::AttachmentState)?;
    if attachment.active_connection != Some(active.connection_id)
        || attachment.terminal_id != active.terminal_id
        || attachment.client_id != active.client_id
    {
        return Err(ConnectionError::AttachmentState);
    }
    Ok(attachment)
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, ConnectionError> {
    mutex.lock().map_err(|_| ConnectionError::AttachmentState)
}

async fn send_replay(
    socket: &mut WebSocket,
    subscription: &TerminalSubscription,
) -> Result<(), axum::Error> {
    match &subscription.replay {
        TerminalReplay::Chunks(chunks) => {
            for chunk in chunks {
                let encoded = encode_terminal_web_binary(&TerminalWebBinaryFrame::Output {
                    sequence: chunk.sequence,
                    bytes: chunk.bytes.to_vec(),
                })
                .expect("PTY chunks are bounded below the web transport limit");
                socket.send(Message::Binary(encoded.into())).await?;
            }
        }
        TerminalReplay::SnapshotRequired(snapshot) => {
            let encoded = encode_terminal_web_binary(&TerminalWebBinaryFrame::Snapshot {
                base_sequence: snapshot.base_sequence,
                next_sequence: snapshot.next_sequence,
                total_bytes: snapshot.total_bytes,
                bytes: snapshot.tail.clone(),
            })
            .expect("terminal snapshot tails are bounded below the web transport limit");
            socket.send(Message::Binary(encoded.into())).await?;
        }
    }
    Ok(())
}

fn spawn_output_pump(
    subscription: TerminalSubscription,
    outbound: mpsc::Sender<Outbound>,
    cancel: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        while !cancel.load(Ordering::Acquire) {
            match subscription.receiver.recv_timeout(SUBSCRIPTION_POLL) {
                Ok(TerminalEvent::Output(chunk)) => {
                    let encoded = encode_terminal_web_binary(&TerminalWebBinaryFrame::Output {
                        sequence: chunk.sequence,
                        bytes: chunk.bytes.to_vec(),
                    })
                    .expect("PTY chunks are bounded below the web transport limit");
                    if outbound.blocking_send(Outbound::Binary(encoded)).is_err() {
                        break;
                    }
                }
                Ok(TerminalEvent::Exited(exit)) => {
                    let _ = outbound.blocking_send(Outbound::Control(
                        TerminalWebServerControl::Exited {
                            exit_code: exit.exit_code,
                            signal: exit.signal,
                        },
                    ));
                    let _ = outbound.blocking_send(Outbound::End);
                    break;
                }
                Ok(TerminalEvent::Fault(message)) => {
                    let _ = outbound.blocking_send(Outbound::Control(
                        TerminalWebServerControl::Error {
                            code: "terminal_fault".into(),
                            message,
                        },
                    ));
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
    })
}

async fn write_socket(
    mut socket: futures_util::stream::SplitSink<WebSocket, Message>,
    mut outbound: mpsc::Receiver<Outbound>,
) {
    while let Some(message) = outbound.recv().await {
        let result = match message {
            Outbound::Binary(bytes) => socket.send(Message::Binary(bytes.into())).await,
            Outbound::Control(control) => {
                let Ok(json) = serde_json::to_string(&control) else {
                    break;
                };
                socket.send(Message::Text(json.into())).await
            }
            Outbound::End => break,
        };
        if result.is_err() {
            break;
        }
    }
    let _ = socket.close().await;
}

async fn read_socket(
    mut socket: futures_util::stream::SplitStream<WebSocket>,
    outbound: mpsc::Sender<Outbound>,
    runtime: GatewayRuntime,
    active: &ActiveAttachment,
) {
    while let Some(message) = socket.next().await {
        let result = match message {
            Ok(Message::Binary(bytes)) => match decode_terminal_web_binary(&bytes) {
                Ok(TerminalWebBinaryFrame::Input { sequence, bytes }) => {
                    runtime.accept_input(active, sequence, bytes)
                }
                Ok(_) => Err(ConnectionError::InvalidMessage(
                    "client may send only binary input frames".into(),
                )),
                Err(error) => Err(ConnectionError::InvalidMessage(error.to_string())),
            },
            Ok(Message::Text(text)) => match serde_json::from_str(&text) {
                Ok(TerminalWebClientControl::Resize { generation, size }) => {
                    runtime.accept_resize(active, generation, size)
                }
                Ok(TerminalWebClientControl::Close) => {
                    let result = runtime.close_attachment(active);
                    if result.is_ok() {
                        let _ = outbound.send(Outbound::End).await;
                    }
                    return;
                }
                Ok(TerminalWebClientControl::Hello { .. }) => Err(ConnectionError::InvalidMessage(
                    "hello may be sent only once".into(),
                )),
                Err(error) => Err(ConnectionError::InvalidMessage(error.to_string())),
            },
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(Message::Ping(_) | Message::Pong(_)) => Ok(()),
        };
        if let Err(error) = result {
            let _ = outbound.send(Outbound::Control(error.control())).await;
            let _ = outbound.send(Outbound::End).await;
            return;
        }
    }
}

async fn send_control(
    socket: &mut WebSocket,
    control: TerminalWebServerControl,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(&control).expect("server controls serialize");
    socket.send(Message::Text(json.into())).await
}

async fn serve_asset(State(runtime): State<GatewayRuntime>, uri: Uri) -> Response {
    let relative = uri.path().trim_start_matches('/');
    let relative = if relative.is_empty() {
        Path::new("index.html")
    } else {
        Path::new(relative)
    };
    if uri.path().contains('%')
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return status_response(StatusCode::BAD_REQUEST, "invalid asset path");
    }
    let candidate = runtime.assets.join(relative);
    let Ok(candidate) = candidate.canonicalize() else {
        return status_response(StatusCode::NOT_FOUND, "asset not found");
    };
    if !candidate.starts_with(runtime.assets.as_ref()) || !candidate.is_file() {
        return status_response(StatusCode::NOT_FOUND, "asset not found");
    }
    let Ok(bytes) = tokio::fs::read(&candidate).await else {
        return status_response(StatusCode::NOT_FOUND, "asset not found");
    };
    let content_type = match candidate.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    };
    secure_response(StatusCode::OK, content_type, Body::from(bytes))
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
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(TERMINAL_CSP),
    );
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

fn constant_time_eq(candidate: &[u8], expected: &[u8]) -> bool {
    if candidate.len() != expected.len() {
        return false;
    }
    let difference = candidate
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        });
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use hyper_term_protocol::{TerminalWebBinaryFrame, TerminalWebServerControl};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    #[tokio::test(flavor = "multi_thread")]
    async fn websocket_drives_the_real_user_shell_and_preserves_binary_output() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let assets = temporary.path().join("assets");
        std::fs::create_dir(&assets).expect("create assets");
        std::fs::write(assets.join("index.html"), "terminal").expect("write asset");
        let daemon = DaemonState::open(temporary.path().join("state")).expect("daemon");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_terminal_gateway(
            TerminalGatewayConfig {
                bind: "127.0.0.1:0".parse().expect("socket"),
                assets,
                token: token.clone(),
                default_cwd: Some(temporary.path().to_owned()),
            },
            daemon,
        )
        .await
        .expect("gateway");

        let origin = format!("http://{}", gateway.address());
        assert_websocket_rejected(
            gateway.address(),
            "0123456789abcdef0123456789abcdee",
            &origin,
            401,
        )
        .await;
        assert_websocket_rejected(gateway.address(), &token, "http://127.0.0.1:9", 403).await;
        let mut request = format!("ws://{}/terminal?token={token}", gateway.address())
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("origin", HeaderValue::from_str(&origin).expect("origin"));
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let stale_attachment_id = TerminalAttachmentId::new();
        socket
            .send(ClientMessage::Text(
                serde_json::to_string(&TerminalWebClientControl::Hello {
                    protocol_version: TERMINAL_WEB_PROTOCOL_VERSION,
                    attachment_id: Some(stale_attachment_id),
                    after_sequence: 0,
                    size: TerminalSize {
                        cols: 80,
                        rows: 24,
                        pixel_width: 800,
                        pixel_height: 480,
                    },
                    cwd: None,
                })
                .expect("hello")
                .into(),
            ))
            .await
            .expect("send hello");

        let (active_attachment_id, next_input_sequence) = loop {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("ready timeout")
                .expect("ready message")
                .expect("ready frame");
            if let ClientMessage::Text(json) = message
                && let TerminalWebServerControl::Ready {
                    attachment_id,
                    next_input_sequence,
                    ..
                } = serde_json::from_str(&json).expect("ready control")
            {
                break (attachment_id, next_input_sequence);
            }
        };
        assert_ne!(active_attachment_id, stale_attachment_id);
        socket
            .send(ClientMessage::Binary(
                encode_terminal_web_binary(&TerminalWebBinaryFrame::Input {
                    sequence: next_input_sequence,
                    bytes: b"printf '\\137\\137HYPER_TERM_GATEWAY_OK\\137\\137:%s\\n' \"$PWD\"\n"
                        .to_vec(),
                })
                .expect("input frame")
                .into(),
            ))
            .await
            .expect("send input");

        let expected = format!(
            "__HYPER_TERM_GATEWAY_OK__:{}",
            temporary
                .path()
                .canonicalize()
                .expect("canonical cwd")
                .display()
        );
        let mut transcript = Vec::new();
        while !String::from_utf8_lossy(&transcript).contains(&expected) {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("output timeout")
                .expect("output message")
                .expect("output frame");
            if let ClientMessage::Binary(bytes) = message {
                match decode_terminal_web_binary(&bytes).expect("terminal output") {
                    TerminalWebBinaryFrame::Output { bytes, .. }
                    | TerminalWebBinaryFrame::Snapshot { bytes, .. } => transcript.extend(bytes),
                    TerminalWebBinaryFrame::Input { .. } => panic!("server sent input"),
                }
            }
        }
        assert!(
            String::from_utf8_lossy(&transcript).contains(&expected),
            "default shell cwd was not applied: {}",
            String::from_utf8_lossy(&transcript)
        );

        socket
            .send(ClientMessage::Text(
                serde_json::to_string(&TerminalWebClientControl::Close)
                    .expect("close")
                    .into(),
            ))
            .await
            .expect("close shell");
        gateway.shutdown().await.expect("shutdown gateway");
    }

    async fn assert_websocket_rejected(
        address: SocketAddr,
        token: &str,
        origin: &str,
        expected_status: u16,
    ) {
        let mut request = format!("ws://{address}/terminal?token={token}")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            "origin",
            HeaderValue::from_str(origin).expect("origin header"),
        );
        let error = connect_async(request).await.expect_err("must reject");
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("expected HTTP rejection, got {error}");
        };
        assert_eq!(response.status().as_u16(), expected_status);
    }

    #[tokio::test]
    async fn gateway_rejects_non_loopback_and_weak_tokens() {
        let daemon = DaemonState::open(tempfile::tempdir().expect("temp").path()).expect("daemon");
        let error = spawn_terminal_gateway(
            TerminalGatewayConfig {
                bind: "0.0.0.0:0".parse().expect("socket"),
                assets: PathBuf::new(),
                token: "short".into(),
                default_cwd: None,
            },
            daemon,
        )
        .await
        .expect_err("public bind must fail");
        assert!(matches!(error, TerminalGatewayError::NonLoopbackBind(_)));
    }

    #[test]
    fn token_comparison_checks_every_equal_length_byte() {
        assert!(constant_time_eq(b"0123456789", b"0123456789"));
        assert!(!constant_time_eq(b"0123456788", b"0123456789"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}
