use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AcceptedGenUiArtifact, ApprovalDetailDigest, BlockDocument, BlockPatch,
    BrokeredMcpToolExecution, ClientId, EventEnvelope, GenUiArtifactCandidate, InputLeaseId,
    OperationAction, OperationCompletion, OperationId, OperationKind, OperationState,
    PROTOCOL_VERSION, PermissionDecision, RequestId, RiskClass, TaskId, TerminalId, TerminalSize,
};

const MAGIC: [u8; 4] = *b"HTRM";
const HEADER_LEN: usize = 12;
pub const MAX_CONTROL_FRAME_BYTES: usize = 3 * 1024 * 1024;
pub const MAX_TERMINAL_FRAME_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum FrameKind {
    Control = 1,
    TerminalInput = 16,
    TerminalOutput = 17,
    TerminalSnapshot = 18,
}

impl TryFrom<u8> for FrameKind {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Control),
            16 => Ok(Self::TerminalInput),
            17 => Ok(Self::TerminalOutput),
            18 => Ok(Self::TerminalSnapshot),
            other => Err(WireError::UnknownFrameKind(other)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlRequestEnvelope {
    pub request_id: RequestId,
    pub request: ControlRequest,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    Hello {
        client_id: ClientId,
        protocol_version: u16,
    },
    CreateTask {
        title: String,
    },
    ProposeOperation {
        task_id: TaskId,
        kind: OperationKind,
        action: OperationAction,
        summary: String,
        risk: RiskClass,
        required_capabilities: Vec<String>,
    },
    DecidePermission {
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        approval_detail_digest: ApprovalDetailDigest,
        decision: PermissionDecision,
    },
    BeginOperation {
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
    },
    ExecuteBrokeredMcpTool {
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        tool_name: String,
        proposal_digest: String,
        arguments: serde_json::Value,
    },
    CompleteOperation {
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        completion: OperationCompletion,
    },
    AcceptGenUiArtifact {
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        candidate: GenUiArtifactCandidate,
    },
    DispatchTerminal {
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        size: TerminalSize,
    },
    OpenUserShell {
        cwd: Option<std::path::PathBuf>,
        size: TerminalSize,
    },
    SubscribeTerminal {
        terminal_id: TerminalId,
        after_sequence: u64,
    },
    ResizeTerminal {
        terminal_id: TerminalId,
        generation: u64,
        size: TerminalSize,
    },
    CloseTerminal {
        terminal_id: TerminalId,
    },
    AcquireInputLease {
        terminal_id: TerminalId,
        client_id: ClientId,
    },
    ReleaseInputLease {
        terminal_id: TerminalId,
        lease_id: InputLeaseId,
    },
    GetBlockSnapshot {
        task_id: TaskId,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ControlResponseEnvelope {
    pub request_id: Option<RequestId>,
    pub response: ControlResponse,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResponse {
    Welcome {
        protocol_version: u16,
        daemon_instance: uuid::Uuid,
    },
    TaskCreated {
        task_id: TaskId,
    },
    OperationUpdated {
        operation_id: OperationId,
        revision: u64,
        state: OperationState,
    },
    GenUiArtifactAccepted {
        artifact: AcceptedGenUiArtifact,
    },
    BrokeredMcpToolExecuted {
        execution: BrokeredMcpToolExecution,
    },
    TerminalCreated {
        terminal_id: TerminalId,
    },
    TerminalSubscribed {
        terminal_id: TerminalId,
        after_sequence: u64,
    },
    InputLeaseGranted {
        terminal_id: TerminalId,
        lease_id: InputLeaseId,
        generation: u64,
    },
    TerminalExited {
        terminal_id: TerminalId,
        exit_code: Option<u32>,
    },
    Event {
        event: Box<EventEnvelope>,
    },
    BlockSnapshot {
        document: BlockDocument,
    },
    BlockPatch {
        patch: BlockPatch,
    },
    Ack,
    Error {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalDataFrame {
    pub terminal_id: TerminalId,
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalInputFrame {
    pub terminal_id: TerminalId,
    pub lease_id: InputLeaseId,
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSnapshotFrame {
    pub terminal_id: TerminalId,
    pub base_sequence: u64,
    pub next_sequence: u64,
    pub total_bytes: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WireFrame {
    Request(ControlRequestEnvelope),
    Response(ControlResponseEnvelope),
    TerminalInput(TerminalInputFrame),
    TerminalOutput(TerminalDataFrame),
    TerminalSnapshot(TerminalSnapshotFrame),
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid wire magic")]
    InvalidMagic,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("unknown frame kind {0}")]
    UnknownFrameKind(u8),
    #[error("frame length {actual} exceeds maximum {maximum}")]
    FrameTooLarge { actual: usize, maximum: usize },
    #[error("terminal frame is shorter than its fixed header")]
    TruncatedTerminalFrame,
    #[error("invalid JSON control frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control frame shape is invalid: {0}")]
    InvalidControlShape(String),
}

#[derive(Serialize)]
#[serde(tag = "direction", content = "payload", rename_all = "snake_case")]
enum ControlWire<'a> {
    Request(&'a ControlRequestEnvelope),
    Response(&'a ControlResponseEnvelope),
}

#[derive(Deserialize)]
#[serde(tag = "direction", content = "payload", rename_all = "snake_case")]
enum OwnedControlWire {
    Request(ControlRequestEnvelope),
    Response(ControlResponseEnvelope),
}

pub fn write_frame(mut writer: impl Write, frame: &WireFrame) -> Result<(), WireError> {
    let (kind, payload) = match frame {
        WireFrame::Request(request) => (
            FrameKind::Control,
            serde_json::to_vec(&ControlWire::Request(request))?,
        ),
        WireFrame::Response(response) => (
            FrameKind::Control,
            serde_json::to_vec(&ControlWire::Response(response))?,
        ),
        WireFrame::TerminalInput(frame) => (FrameKind::TerminalInput, encode_terminal_input(frame)),
        WireFrame::TerminalOutput(frame) => {
            (FrameKind::TerminalOutput, encode_terminal_data(frame))
        }
        WireFrame::TerminalSnapshot(frame) => {
            (FrameKind::TerminalSnapshot, encode_terminal_snapshot(frame))
        }
    };

    let maximum = if kind == FrameKind::Control {
        MAX_CONTROL_FRAME_BYTES
    } else {
        MAX_TERMINAL_FRAME_BYTES
    };
    if payload.len() > maximum {
        return Err(WireError::FrameTooLarge {
            actual: payload.len(),
            maximum,
        });
    }

    let mut header = [0_u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    header[6] = kind as u8;
    header[7] = 0;
    header[8..12].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame(mut reader: impl Read) -> Result<WireFrame, WireError> {
    let mut header = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if header[..4] != MAGIC {
        return Err(WireError::InvalidMagic);
    }
    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != PROTOCOL_VERSION {
        return Err(WireError::UnsupportedVersion(version));
    }
    let kind = FrameKind::try_from(header[6])?;
    let length = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    let maximum = if kind == FrameKind::Control {
        MAX_CONTROL_FRAME_BYTES
    } else {
        MAX_TERMINAL_FRAME_BYTES
    };
    if length > maximum {
        return Err(WireError::FrameTooLarge {
            actual: length,
            maximum,
        });
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;

    match kind {
        FrameKind::Control => match serde_json::from_slice::<OwnedControlWire>(&payload)? {
            OwnedControlWire::Request(request) => Ok(WireFrame::Request(request)),
            OwnedControlWire::Response(response) => Ok(WireFrame::Response(response)),
        },
        FrameKind::TerminalInput => Ok(WireFrame::TerminalInput(decode_terminal_input(&payload)?)),
        FrameKind::TerminalOutput => Ok(WireFrame::TerminalOutput(decode_terminal_data(&payload)?)),
        FrameKind::TerminalSnapshot => Ok(WireFrame::TerminalSnapshot(decode_terminal_snapshot(
            &payload,
        )?)),
    }
}

fn encode_terminal_data(frame: &TerminalDataFrame) -> Vec<u8> {
    let mut payload = Vec::with_capacity(24 + frame.bytes.len());
    payload.extend_from_slice(frame.terminal_id.as_uuid().as_bytes());
    payload.extend_from_slice(&frame.sequence.to_be_bytes());
    payload.extend_from_slice(&frame.bytes);
    payload
}

fn encode_terminal_input(frame: &TerminalInputFrame) -> Vec<u8> {
    let mut payload = Vec::with_capacity(40 + frame.bytes.len());
    payload.extend_from_slice(frame.terminal_id.as_uuid().as_bytes());
    payload.extend_from_slice(frame.lease_id.as_uuid().as_bytes());
    payload.extend_from_slice(&frame.sequence.to_be_bytes());
    payload.extend_from_slice(&frame.bytes);
    payload
}

fn decode_terminal_input(payload: &[u8]) -> Result<TerminalInputFrame, WireError> {
    if payload.len() < 40 {
        return Err(WireError::TruncatedTerminalFrame);
    }
    let terminal_id = uuid::Uuid::from_slice(&payload[..16])
        .map_err(|error| WireError::InvalidControlShape(error.to_string()))?;
    let lease_id = uuid::Uuid::from_slice(&payload[16..32])
        .map_err(|error| WireError::InvalidControlShape(error.to_string()))?;
    let sequence = u64::from_be_bytes(payload[32..40].try_into().expect("fixed slice"));
    Ok(TerminalInputFrame {
        terminal_id: TerminalId::from_uuid(terminal_id),
        lease_id: InputLeaseId::from_uuid(lease_id),
        sequence,
        bytes: payload[40..].to_vec(),
    })
}

fn decode_terminal_data(payload: &[u8]) -> Result<TerminalDataFrame, WireError> {
    if payload.len() < 24 {
        return Err(WireError::TruncatedTerminalFrame);
    }
    let id = uuid::Uuid::from_slice(&payload[..16])
        .map_err(|error| WireError::InvalidControlShape(error.to_string()))?;
    let sequence = u64::from_be_bytes(payload[16..24].try_into().expect("fixed slice"));
    Ok(TerminalDataFrame {
        terminal_id: TerminalId::from_uuid(id),
        sequence,
        bytes: payload[24..].to_vec(),
    })
}

fn encode_terminal_snapshot(frame: &TerminalSnapshotFrame) -> Vec<u8> {
    let mut payload = Vec::with_capacity(40 + frame.bytes.len());
    payload.extend_from_slice(frame.terminal_id.as_uuid().as_bytes());
    payload.extend_from_slice(&frame.base_sequence.to_be_bytes());
    payload.extend_from_slice(&frame.next_sequence.to_be_bytes());
    payload.extend_from_slice(&frame.total_bytes.to_be_bytes());
    payload.extend_from_slice(&frame.bytes);
    payload
}

fn decode_terminal_snapshot(payload: &[u8]) -> Result<TerminalSnapshotFrame, WireError> {
    if payload.len() < 40 {
        return Err(WireError::TruncatedTerminalFrame);
    }
    let id = uuid::Uuid::from_slice(&payload[..16])
        .map_err(|error| WireError::InvalidControlShape(error.to_string()))?;
    let base_sequence = u64::from_be_bytes(payload[16..24].try_into().expect("fixed slice"));
    let next_sequence = u64::from_be_bytes(payload[24..32].try_into().expect("fixed slice"));
    let total_bytes = u64::from_be_bytes(payload[32..40].try_into().expect("fixed slice"));
    Ok(TerminalSnapshotFrame {
        terminal_id: TerminalId::from_uuid(id),
        base_sequence,
        next_sequence,
        total_bytes,
        bytes: payload[40..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OperationOutcome;

    #[test]
    fn binary_terminal_frame_round_trips_without_base64() {
        let original = WireFrame::TerminalOutput(TerminalDataFrame {
            terminal_id: TerminalId::new(),
            sequence: 42,
            bytes: vec![0, 1, 2, 0xff],
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &original).expect("encode");
        let decoded = read_frame(bytes.as_slice()).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn binary_terminal_input_carries_lease_and_sequence() {
        let original = WireFrame::TerminalInput(TerminalInputFrame {
            terminal_id: TerminalId::new(),
            lease_id: InputLeaseId::new(),
            sequence: 7,
            bytes: b"cargo test\n".to_vec(),
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &original).expect("encode");
        assert_eq!(read_frame(bytes.as_slice()).expect("decode"), original);
    }

    #[test]
    fn control_frame_round_trips_with_a_versioned_shape() {
        let original = WireFrame::Request(ControlRequestEnvelope {
            request_id: RequestId::new(),
            request: ControlRequest::CreateTask {
                title: "test task".into(),
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &original).expect("encode");
        let decoded = read_frame(bytes.as_slice()).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn brokered_operation_completion_round_trips_as_structured_evidence() {
        let original = WireFrame::Request(ControlRequestEnvelope {
            request_id: RequestId::new(),
            request: ControlRequest::CompleteOperation {
                task_id: TaskId::new(),
                operation_id: OperationId::new(),
                expected_revision: 5,
                completion: OperationCompletion {
                    executor: "hyper-term-mcp".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: "diff ready".into(),
                    result_digest: Some("a".repeat(64)),
                },
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &original).expect("encode");
        assert_eq!(read_frame(bytes.as_slice()).expect("decode"), original);
    }

    #[test]
    fn brokered_mcp_execution_request_round_trips_with_operation_binding() {
        let original = WireFrame::Request(ControlRequestEnvelope {
            request_id: RequestId::new(),
            request: ControlRequest::ExecuteBrokeredMcpTool {
                task_id: TaskId::new(),
                operation_id: OperationId::new(),
                expected_revision: 4,
                tool_name: "hyper_term.genui.compile".into(),
                proposal_digest: "a".repeat(64),
                arguments: serde_json::json!({
                    "source": "export default function App(){ return <main />; }",
                    "entry": "App.tsx"
                }),
            },
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &original).expect("encode");
        assert_eq!(read_frame(bytes.as_slice()).expect("decode"), original);
    }

    #[test]
    fn user_shell_request_cannot_choose_a_program_or_environment() {
        let request = ControlRequest::OpenUserShell {
            cwd: Some(std::path::PathBuf::from("/tmp/project")),
            size: TerminalSize::default(),
        };
        let value = serde_json::to_value(request).expect("serialize user shell request");
        assert_eq!(value["type"], "open_user_shell");
        assert_eq!(value["cwd"], "/tmp/project");
        assert!(value.get("program").is_none());
        assert!(value.get("args").is_none());
        assert!(value.get("env").is_none());
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let mut header = [0_u8; HEADER_LEN];
        header[..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        header[6] = FrameKind::TerminalOutput as u8;
        header[8..12].copy_from_slice(&((MAX_TERMINAL_FRAME_BYTES as u32) + 1).to_be_bytes());

        let error = read_frame(header.as_slice()).expect_err("must reject oversized frame");
        assert!(matches!(error, WireError::FrameTooLarge { .. }));
    }
}
