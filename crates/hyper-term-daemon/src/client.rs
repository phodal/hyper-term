use std::collections::VecDeque;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, bounded};
use hyper_term_protocol::{
    ClientId, ControlRequest, ControlRequestEnvelope, ControlResponse, PROTOCOL_VERSION, RequestId,
    WireError, WireFrame, read_frame, write_frame,
};
use thiserror::Error;

const CLIENT_FRAME_CAPACITY: usize = 512;
const CLIENT_PENDING_CAPACITY: usize = 512;

/// A blocking, framed client for hyperd's Unix control plane.
///
/// The reader runs independently so broadcast events can safely interleave
/// with request responses without corrupting the ordered wire stream.
pub struct ControlClient {
    client_id: ClientId,
    writer: UnixStream,
    frames: Receiver<Result<WireFrame, WireError>>,
    pending: VecDeque<WireFrame>,
    reader: Option<JoinHandle<()>>,
}

impl ControlClient {
    pub fn connect(path: impl AsRef<Path>, timeout: Duration) -> Result<Self, ControlClientError> {
        if timeout.is_zero() {
            return Err(ControlClientError::InvalidTimeout);
        }
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let client_id = ClientId::new();
        let hello_id = RequestId::new();
        write_frame(
            &mut stream,
            &WireFrame::Request(ControlRequestEnvelope {
                request_id: hello_id,
                request: ControlRequest::Hello {
                    client_id,
                    protocol_version: PROTOCOL_VERSION,
                },
            }),
        )?;
        let welcome = read_frame(&mut stream)?;
        match welcome {
            WireFrame::Response(response)
                if response.request_id == Some(hello_id)
                    && matches!(
                        response.response,
                        ControlResponse::Welcome {
                            protocol_version: PROTOCOL_VERSION,
                            ..
                        }
                    ) => {}
            WireFrame::Response(response) => {
                return Err(ControlClientError::HandshakeRejected(Box::new(
                    response.response,
                )));
            }
            frame => {
                return Err(ControlClientError::UnexpectedHandshake(Box::new(frame)));
            }
        }
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        let mut reader_stream = stream.try_clone()?;
        let (sender, frames) = bounded(CLIENT_FRAME_CAPACITY);
        let reader = thread::Builder::new()
            .name(format!("hyperd-control-reader-{client_id}"))
            .spawn(move || {
                loop {
                    let frame = read_frame(&mut reader_stream);
                    let terminal = frame.is_err();
                    if sender.send(frame).is_err() || terminal {
                        break;
                    }
                }
            })?;
        Ok(Self {
            client_id,
            writer: stream,
            frames,
            pending: VecDeque::new(),
            reader: Some(reader),
        })
    }

    pub fn client_id(&self) -> ClientId {
        self.client_id
    }

    pub fn request(
        &mut self,
        request: ControlRequest,
        timeout: Duration,
    ) -> Result<ControlResponse, ControlClientError> {
        if timeout.is_zero() {
            return Err(ControlClientError::InvalidTimeout);
        }
        let request_id = RequestId::new();
        write_frame(
            &mut self.writer,
            &WireFrame::Request(ControlRequestEnvelope {
                request_id,
                request,
            }),
        )?;
        let deadline = Instant::now() + timeout;
        let mut deferred = std::mem::take(&mut self.pending);
        let result = self.wait_for_response(request_id, deadline, &mut deferred);
        self.pending = deferred;
        result
    }

    fn wait_for_response(
        &self,
        request_id: RequestId,
        deadline: Instant,
        deferred: &mut VecDeque<WireFrame>,
    ) -> Result<ControlResponse, ControlClientError> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ControlClientError::Timeout);
            }
            let frame = self.receive_frame(remaining)?;
            match frame {
                WireFrame::Response(response) if response.request_id == Some(request_id) => {
                    return Ok(response.response);
                }
                frame => push_pending(deferred, frame)?,
            }
        }
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<WireFrame, ControlClientError> {
        if timeout.is_zero() {
            return Err(ControlClientError::InvalidTimeout);
        }
        self.next_frame(timeout)
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<WireFrame, ControlClientError> {
        if let Some(frame) = self.pending.pop_front() {
            return Ok(frame);
        }
        self.receive_frame(timeout)
    }

    fn receive_frame(&self, timeout: Duration) -> Result<WireFrame, ControlClientError> {
        match self.frames.recv_timeout(timeout) {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(error)) => Err(error.into()),
            Err(RecvTimeoutError::Timeout) => Err(ControlClientError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(ControlClientError::Disconnected),
        }
    }
}

fn push_pending(
    pending: &mut VecDeque<WireFrame>,
    frame: WireFrame,
) -> Result<(), ControlClientError> {
    if pending.len() == CLIENT_PENDING_CAPACITY {
        return Err(ControlClientError::PendingOverflow);
    }
    pending.push_back(frame);
    Ok(())
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[derive(Debug, Error)]
pub enum ControlClientError {
    #[error("hyperd control client I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("hyperd control frame failed: {0}")]
    Wire(#[from] WireError),
    #[error("hyperd rejected the control handshake: {0:?}")]
    HandshakeRejected(Box<ControlResponse>),
    #[error("hyperd sent an unexpected handshake frame: {0:?}")]
    UnexpectedHandshake(Box<WireFrame>),
    #[error("hyperd control request timed out")]
    Timeout,
    #[error("hyperd control stream disconnected")]
    Disconnected,
    #[error("hyperd control client pending queue exceeded its bound")]
    PendingOverflow,
    #[error("hyperd control timeout must be positive")]
    InvalidTimeout,
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::DomainEvent;
    use tempfile::tempdir;

    use crate::{DaemonState, spawn_unix_server};

    use super::*;

    #[test]
    fn request_response_preserves_interleaved_authority_events() {
        let directory = tempdir().unwrap();
        let socket = directory.path().join("hyperd.sock");
        let state = DaemonState::open(directory.path().join("state")).unwrap();
        let _server = spawn_unix_server(&socket, state).unwrap();
        let mut client = ControlClient::connect(&socket, Duration::from_secs(1)).unwrap();
        let task_id = match client
            .request(
                ControlRequest::CreateTask {
                    title: "control client".into(),
                },
                Duration::from_secs(1),
            )
            .unwrap()
        {
            ControlResponse::TaskCreated { task_id } => task_id,
            response => panic!("unexpected response: {response:?}"),
        };

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "task event was not preserved");
            if let WireFrame::Response(response) = client.recv_timeout(remaining).unwrap()
                && let ControlResponse::Event { event } = response.response
                && event.task_id == task_id
                && matches!(event.payload, DomainEvent::TaskCreated { .. })
            {
                break;
            }
        }
    }
}
