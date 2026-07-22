use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;

#[derive(Clone, Copy)]
enum ConnectionAuthority {
    DesktopController,
    AgentMcpConnector { task_id: TaskId },
}

impl ConnectionAuthority {
    fn allows_request(self, request: &ControlRequest) -> bool {
        match self {
            Self::DesktopController => true,
            Self::AgentMcpConnector { task_id: bound } => match request {
                ControlRequest::ProposeOperation {
                    task_id,
                    kind: OperationKind::McpTool,
                    action: OperationAction::Opaque { .. },
                    ..
                }
                | ControlRequest::BeginOperation { task_id, .. }
                | ControlRequest::ExecuteBrokeredMcpTool { task_id, .. }
                | ControlRequest::CompleteOperation { task_id, .. }
                | ControlRequest::AcceptGenUiArtifact { task_id, .. } => *task_id == bound,
                ControlRequest::Hello { .. }
                | ControlRequest::CreateTask { .. }
                | ControlRequest::ProposeOperation { .. }
                | ControlRequest::DecidePermission { .. }
                | ControlRequest::DispatchTerminal { .. }
                | ControlRequest::OpenUserShell { .. }
                | ControlRequest::SubscribeTerminal { .. }
                | ControlRequest::ResizeTerminal { .. }
                | ControlRequest::CloseTerminal { .. }
                | ControlRequest::AcquireInputLease { .. }
                | ControlRequest::ReleaseInputLease { .. }
                | ControlRequest::GetBlockSnapshot { .. } => false,
            },
        }
    }

    fn allows_terminal_input(self) -> bool {
        matches!(self, Self::DesktopController)
    }

    fn allows_broadcast(self, response: &ControlResponse) -> bool {
        match self {
            Self::DesktopController => true,
            Self::AgentMcpConnector { task_id } => matches!(
                response,
                ControlResponse::Event { event } if event.task_id == task_id
            ),
        }
    }
}

pub struct UnixServerHandle {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for UnixServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

pub fn spawn_unix_server(
    path: impl AsRef<Path>,
    state: DaemonState,
) -> Result<UnixServerHandle, DaemonError> {
    spawn_server(path, state, ConnectionAuthority::DesktopController)
}

/// Starts a server-assigned, task-scoped endpoint for the MCP connector inside
/// an Agent provider sandbox. The connector cannot promote itself to desktop
/// authority through the wire handshake.
pub fn spawn_agent_capability_server(
    path: impl AsRef<Path>,
    state: DaemonState,
    task_id: TaskId,
) -> Result<UnixServerHandle, DaemonError> {
    spawn_server(
        path,
        state,
        ConnectionAuthority::AgentMcpConnector { task_id },
    )
}

fn spawn_server(
    path: impl AsRef<Path>,
    state: DaemonState,
    authority: ConnectionAuthority,
) -> Result<UnixServerHandle, DaemonError> {
    let path = path.as_ref().to_path_buf();
    let listener = bind_socket(&path)?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("hyperd-accept".into())
        .spawn(move || accept_until_stopped(listener, state, authority, thread_stop))?;
    Ok(UnixServerHandle {
        path,
        stop,
        thread: Some(thread),
    })
}

pub fn run_unix_server(path: impl AsRef<Path>, state: DaemonState) -> Result<(), DaemonError> {
    let path = path.as_ref().to_path_buf();
    let listener = bind_socket(&path)?;
    let _cleanup = SocketCleanup(path);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => spawn_connection(
                stream,
                state.clone(),
                ConnectionAuthority::DesktopController,
            )?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn bind_socket(path: &Path) -> Result<UnixListener, DaemonError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(DaemonError::UnsafeSocketPath(path.to_path_buf()));
        }
        if UnixStream::connect(path).is_ok() {
            return Err(DaemonError::SocketInUse(path.to_path_buf()));
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn accept_until_stopped(
    listener: UnixListener,
    state: DaemonState,
    authority: ConnectionAuthority,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = spawn_connection(stream, state.clone(), authority);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

fn spawn_connection(
    stream: UnixStream,
    state: DaemonState,
    authority: ConnectionAuthority,
) -> Result<(), DaemonError> {
    // A nonblocking listener can yield a nonblocking accepted socket on
    // some Unix hosts. Each connection has its own reader thread, so keep
    // the framed protocol blocking and avoid treating a handshake race as
    // an invalid frame.
    stream.set_nonblocking(false)?;
    thread::Builder::new()
        .name("hyperd-client".into())
        .spawn(move || {
            let _ = handle_connection(stream, state, authority);
        })?;
    Ok(())
}

#[derive(Clone)]
struct ConnectionWriter {
    stream: Arc<Mutex<UnixStream>>,
}

impl ConnectionWriter {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream: Arc::new(Mutex::new(stream)),
        }
    }

    fn send(&self, frame: &WireFrame) -> Result<(), DaemonError> {
        write_frame(&mut *lock(&self.stream)?, frame)?;
        Ok(())
    }

    fn response(
        &self,
        request_id: Option<RequestId>,
        response: ControlResponse,
    ) -> Result<(), DaemonError> {
        self.send(&WireFrame::Response(ControlResponseEnvelope {
            request_id,
            response,
        }))
    }
}

fn handle_connection(
    stream: UnixStream,
    state: DaemonState,
    authority: ConnectionAuthority,
) -> Result<(), DaemonError> {
    let mut reader = stream.try_clone()?;
    let writer = ConnectionWriter::new(stream);
    let (client_id, hello_request) = match read_frame(&mut reader)? {
        WireFrame::Request(ControlRequestEnvelope {
            request_id,
            request:
                ControlRequest::Hello {
                    client_id,
                    protocol_version,
                },
        }) => {
            if protocol_version != PROTOCOL_VERSION {
                writer.response(
                    Some(request_id),
                    ControlResponse::Error {
                        code: "unsupported_protocol".into(),
                        message: format!(
                            "client requested {protocol_version}, daemon supports {PROTOCOL_VERSION}"
                        ),
                    },
                )?;
                return Ok(());
            }
            (client_id, request_id)
        }
        _ => return Err(DaemonError::HelloRequired),
    };
    // Register the event stream before acknowledging the handshake. A
    // client may act as soon as `connect` returns, so sending `Welcome`
    // first leaves a window where authority events can be lost.
    let control = state.subscribe_control()?;
    writer.response(
        Some(hello_request),
        ControlResponse::Welcome {
            protocol_version: PROTOCOL_VERSION,
            daemon_instance: state.instance_id(),
        },
    )?;
    let _lease_cleanup = ClientLeaseCleanup {
        state: state.clone(),
        client_id,
    };

    let control_writer = writer.clone();
    thread::Builder::new()
        .name(format!("hyperd-events-{client_id}"))
        .spawn(move || {
            while let Ok(response) = control.recv() {
                if !authority.allows_broadcast(&response) {
                    continue;
                }
                if control_writer.response(None, response).is_err() {
                    break;
                }
            }
        })?;

    loop {
        let frame = match read_frame(&mut reader) {
            Ok(frame) => frame,
            Err(WireError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                break;
            }
            Err(error) => {
                let _ = writer.response(
                    None,
                    ControlResponse::Error {
                        code: "invalid_frame".into(),
                        message: error.to_string(),
                    },
                );
                break;
            }
        };
        match frame {
            WireFrame::Request(request) => {
                handle_request(&state, &writer, client_id, authority, request)?;
            }
            WireFrame::TerminalInput(frame) => {
                if !authority.allows_terminal_input() {
                    writer.response(None, authority_denied())?;
                } else if let Err(error) = state.write_terminal_input(client_id, frame) {
                    writer.response(None, error_response(&error))?;
                }
            }
            WireFrame::Response(_)
            | WireFrame::TerminalOutput(_)
            | WireFrame::TerminalSnapshot(_) => {
                writer.response(
                    None,
                    ControlResponse::Error {
                        code: "invalid_client_frame".into(),
                        message: "client sent a daemon-only frame".into(),
                    },
                )?;
            }
        }
    }
    Ok(())
}

struct ClientLeaseCleanup {
    state: DaemonState,
    client_id: ClientId,
}

impl Drop for ClientLeaseCleanup {
    fn drop(&mut self) {
        self.state.release_client(self.client_id);
    }
}

fn handle_request(
    state: &DaemonState,
    writer: &ConnectionWriter,
    session_client_id: ClientId,
    authority: ConnectionAuthority,
    envelope: ControlRequestEnvelope,
) -> Result<(), DaemonError> {
    let request_id = envelope.request_id;
    if !authority.allows_request(&envelope.request) {
        writer.response(Some(request_id), authority_denied())?;
        return Ok(());
    }
    if let ControlRequest::SubscribeTerminal {
        terminal_id,
        after_sequence,
    } = envelope.request
    {
        match state.terminal_subscription(terminal_id, after_sequence) {
            Ok(subscription) => {
                writer.response(
                    Some(request_id),
                    ControlResponse::TerminalSubscribed {
                        terminal_id,
                        after_sequence,
                    },
                )?;
                spawn_terminal_forwarder(writer.clone(), terminal_id, subscription)?;
            }
            Err(error) => writer.response(Some(request_id), error_response(&error))?,
        }
        return Ok(());
    }

    let response =
        match envelope.request {
            ControlRequest::Hello { .. } => Err(DaemonError::DuplicateHello),
            ControlRequest::CreateTask { title } => state
                .create_task(title)
                .map(|task_id| ControlResponse::TaskCreated { task_id }),
            ControlRequest::ProposeOperation {
                task_id,
                kind,
                action,
                summary,
                risk,
                required_capabilities,
            } => state
                .propose_operation(task_id, kind, action, summary, risk, required_capabilities)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::DecidePermission {
                task_id,
                operation_id,
                expected_revision,
                approval_detail_digest,
                decision,
            } => state
                .decide_permission_bound(
                    task_id,
                    operation_id,
                    expected_revision,
                    &approval_detail_digest,
                    decision,
                )
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::BeginOperation {
                task_id,
                operation_id,
                expected_revision,
            } => state
                .begin_operation(task_id, operation_id, expected_revision)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::ExecuteBrokeredMcpTool {
                task_id,
                operation_id,
                expected_revision,
                tool_name,
                proposal_digest,
                arguments,
            } => state
                .execute_brokered_mcp_tool(
                    task_id,
                    operation_id,
                    expected_revision,
                    tool_name,
                    proposal_digest,
                    arguments,
                )
                .map(|execution| ControlResponse::BrokeredMcpToolExecuted { execution }),
            ControlRequest::CompleteOperation {
                task_id,
                operation_id,
                expected_revision,
                completion,
            } => state
                .complete_operation(task_id, operation_id, expected_revision, completion)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::AcceptGenUiArtifact {
                task_id,
                operation_id,
                expected_revision,
                candidate,
            } => state
                .accept_genui_artifact(task_id, operation_id, expected_revision, candidate)
                .map(|artifact| ControlResponse::GenUiArtifactAccepted { artifact }),
            ControlRequest::DispatchTerminal {
                task_id,
                operation_id,
                expected_revision,
                size,
            } => state
                .dispatch_terminal(task_id, operation_id, expected_revision, size)
                .map(|terminal_id| ControlResponse::TerminalCreated { terminal_id }),
            ControlRequest::OpenUserShell { cwd, size } => state
                .open_user_shell(cwd, size)
                .map(|terminal_id| ControlResponse::TerminalCreated { terminal_id }),
            ControlRequest::ResizeTerminal {
                terminal_id,
                generation,
                size,
            } => state
                .resize_terminal(terminal_id, generation, size)
                .map(|()| ControlResponse::Ack),
            ControlRequest::CloseTerminal { terminal_id } => state
                .close_terminal(terminal_id)
                .map(|()| ControlResponse::Ack),
            ControlRequest::AcquireInputLease {
                terminal_id,
                client_id,
            } => {
                if client_id != session_client_id {
                    Err(DaemonError::ClientIdentityMismatch)
                } else {
                    state.acquire_input_lease(terminal_id, client_id).map(
                        |(lease_id, generation)| ControlResponse::InputLeaseGranted {
                            terminal_id,
                            lease_id,
                            generation,
                        },
                    )
                }
            }
            ControlRequest::ReleaseInputLease {
                terminal_id,
                lease_id,
            } => state
                .release_input_lease(terminal_id, lease_id, session_client_id)
                .map(|()| ControlResponse::Ack),
            ControlRequest::GetBlockSnapshot { task_id } => state
                .block_snapshot(task_id)
                .map(|document| ControlResponse::BlockSnapshot { document }),
            ControlRequest::SubscribeTerminal { .. } => unreachable!("handled above"),
        };
    writer.response(
        Some(request_id),
        match response {
            Ok(response) => response,
            Err(error) => error_response(&error),
        },
    )?;
    Ok(())
}

fn spawn_terminal_forwarder(
    writer: ConnectionWriter,
    terminal_id: TerminalId,
    subscription: TerminalSubscription,
) -> Result<(), DaemonError> {
    thread::Builder::new()
        .name(format!("hyperd-stream-{terminal_id}"))
        .spawn(move || {
            let replay_result = match subscription.replay {
                TerminalReplay::Chunks(chunks) => chunks.into_iter().try_for_each(|chunk| {
                    writer.send(&WireFrame::TerminalOutput(TerminalDataFrame {
                        terminal_id,
                        sequence: chunk.sequence,
                        bytes: chunk.bytes.to_vec(),
                    }))
                }),
                TerminalReplay::SnapshotRequired(snapshot) => {
                    writer.send(&WireFrame::TerminalSnapshot(TerminalSnapshotFrame {
                        terminal_id,
                        base_sequence: snapshot.base_sequence,
                        next_sequence: snapshot.next_sequence,
                        total_bytes: snapshot.total_bytes,
                        bytes: snapshot.tail,
                    }))
                }
            };
            if replay_result.is_err() {
                return;
            }
            if let Some(exit) = subscription.exit {
                let _ = writer.response(
                    None,
                    ControlResponse::TerminalExited {
                        terminal_id,
                        exit_code: exit.exit_code,
                    },
                );
                return;
            }
            while let Ok(event) = subscription.receiver.recv() {
                let result = match event {
                    TerminalEvent::Output(chunk) => {
                        writer.send(&WireFrame::TerminalOutput(TerminalDataFrame {
                            terminal_id,
                            sequence: chunk.sequence,
                            bytes: chunk.bytes.to_vec(),
                        }))
                    }
                    TerminalEvent::Exited(exit) => {
                        let result = writer.response(
                            None,
                            ControlResponse::TerminalExited {
                                terminal_id,
                                exit_code: exit.exit_code,
                            },
                        );
                        let _ = result;
                        break;
                    }
                    TerminalEvent::Fault(message) => writer.response(
                        None,
                        ControlResponse::Error {
                            code: "terminal_fault".into(),
                            message,
                        },
                    ),
                };
                if result.is_err() {
                    break;
                }
            }
        })?;
    Ok(())
}

fn error_response(error: &DaemonError) -> ControlResponse {
    ControlResponse::Error {
        code: error.code().into(),
        message: error.to_string(),
    }
}

fn authority_denied() -> ControlResponse {
    ControlResponse::Error {
        code: "authority_denied".into(),
        message: "request is not allowed for this connection".into(),
    }
}
