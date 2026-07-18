use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{TerminalAttachmentId, TerminalId, TerminalSize};

const MAGIC: [u8; 4] = *b"HTWS";
const HEADER_LEN: usize = 36;
pub const TERMINAL_WEB_PROTOCOL_VERSION: u16 = 1;
pub const MAX_TERMINAL_WEB_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalWebClientControl {
    Hello {
        protocol_version: u16,
        attachment_id: Option<TerminalAttachmentId>,
        after_sequence: u64,
        size: TerminalSize,
        cwd: Option<PathBuf>,
    },
    Resize {
        generation: u64,
        size: TerminalSize,
    },
    Close,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalWebServerControl {
    Ready {
        protocol_version: u16,
        attachment_id: TerminalAttachmentId,
        terminal_id: TerminalId,
        next_input_sequence: u64,
        resize_generation: u64,
    },
    Exited {
        exit_code: Option<u32>,
        signal: Option<String>,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalWebBinaryFrame {
    Input {
        sequence: u64,
        bytes: Vec<u8>,
    },
    Output {
        sequence: u64,
        bytes: Vec<u8>,
    },
    Snapshot {
        base_sequence: u64,
        next_sequence: u64,
        total_bytes: u64,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum BinaryKind {
    Input = 1,
    Output = 2,
    Snapshot = 3,
}

impl TryFrom<u8> for BinaryKind {
    type Error = TerminalWebError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Input),
            2 => Ok(Self::Output),
            3 => Ok(Self::Snapshot),
            other => Err(TerminalWebError::UnknownFrameKind(other)),
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TerminalWebError {
    #[error("terminal web frame is shorter than its fixed header")]
    TruncatedFrame,
    #[error("invalid terminal web frame magic")]
    InvalidMagic,
    #[error("unsupported terminal web protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("unknown terminal web frame kind {0}")]
    UnknownFrameKind(u8),
    #[error("terminal web frame has unsupported flags {0}")]
    UnsupportedFlags(u8),
    #[error("terminal web payload length {actual} exceeds maximum {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("terminal web frame declares {declared} payload bytes but carries {actual}")]
    PayloadLengthMismatch { declared: usize, actual: usize },
    #[error("terminal web {kind} frame contains non-zero unused metadata")]
    UnexpectedMetadata { kind: &'static str },
}

pub fn encode_terminal_web_binary(
    frame: &TerminalWebBinaryFrame,
) -> Result<Vec<u8>, TerminalWebError> {
    let (kind, primary, secondary, total_bytes, bytes) = match frame {
        TerminalWebBinaryFrame::Input { sequence, bytes } => {
            (BinaryKind::Input, *sequence, 0, 0, bytes)
        }
        TerminalWebBinaryFrame::Output { sequence, bytes } => {
            (BinaryKind::Output, *sequence, 0, 0, bytes)
        }
        TerminalWebBinaryFrame::Snapshot {
            base_sequence,
            next_sequence,
            total_bytes,
            bytes,
        } => (
            BinaryKind::Snapshot,
            *base_sequence,
            *next_sequence,
            *total_bytes,
            bytes,
        ),
    };
    if bytes.len() > MAX_TERMINAL_WEB_PAYLOAD_BYTES {
        return Err(TerminalWebError::PayloadTooLarge {
            actual: bytes.len(),
            maximum: MAX_TERMINAL_WEB_PAYLOAD_BYTES,
        });
    }

    let mut encoded = Vec::with_capacity(HEADER_LEN + bytes.len());
    encoded.extend_from_slice(&MAGIC);
    encoded.extend_from_slice(&TERMINAL_WEB_PROTOCOL_VERSION.to_be_bytes());
    encoded.push(kind as u8);
    encoded.push(0);
    encoded.extend_from_slice(&primary.to_be_bytes());
    encoded.extend_from_slice(&secondary.to_be_bytes());
    encoded.extend_from_slice(&total_bytes.to_be_bytes());
    encoded.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    encoded.extend_from_slice(bytes);
    Ok(encoded)
}

pub fn decode_terminal_web_binary(
    bytes: &[u8],
) -> Result<TerminalWebBinaryFrame, TerminalWebError> {
    if bytes.len() < HEADER_LEN {
        return Err(TerminalWebError::TruncatedFrame);
    }
    if bytes[..4] != MAGIC {
        return Err(TerminalWebError::InvalidMagic);
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != TERMINAL_WEB_PROTOCOL_VERSION {
        return Err(TerminalWebError::UnsupportedVersion(version));
    }
    let kind = BinaryKind::try_from(bytes[6])?;
    if bytes[7] != 0 {
        return Err(TerminalWebError::UnsupportedFlags(bytes[7]));
    }
    let primary = read_u64(bytes, 8);
    let secondary = read_u64(bytes, 16);
    let total_bytes = read_u64(bytes, 24);
    let declared = u32::from_be_bytes(bytes[32..36].try_into().expect("fixed slice")) as usize;
    if declared > MAX_TERMINAL_WEB_PAYLOAD_BYTES {
        return Err(TerminalWebError::PayloadTooLarge {
            actual: declared,
            maximum: MAX_TERMINAL_WEB_PAYLOAD_BYTES,
        });
    }
    let payload = &bytes[HEADER_LEN..];
    if declared != payload.len() {
        return Err(TerminalWebError::PayloadLengthMismatch {
            declared,
            actual: payload.len(),
        });
    }

    match kind {
        BinaryKind::Input => {
            require_unused_metadata("input", secondary, total_bytes)?;
            Ok(TerminalWebBinaryFrame::Input {
                sequence: primary,
                bytes: payload.to_vec(),
            })
        }
        BinaryKind::Output => {
            require_unused_metadata("output", secondary, total_bytes)?;
            Ok(TerminalWebBinaryFrame::Output {
                sequence: primary,
                bytes: payload.to_vec(),
            })
        }
        BinaryKind::Snapshot => Ok(TerminalWebBinaryFrame::Snapshot {
            base_sequence: primary,
            next_sequence: secondary,
            total_bytes,
            bytes: payload.to_vec(),
        }),
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().expect("fixed slice"))
}

fn require_unused_metadata(
    kind: &'static str,
    secondary: u64,
    total_bytes: u64,
) -> Result<(), TerminalWebError> {
    if secondary == 0 && total_bytes == 0 {
        Ok(())
    } else {
        Err(TerminalWebError::UnexpectedMetadata { kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_frames_round_trip_without_json_or_base64() {
        let frames = [
            TerminalWebBinaryFrame::Input {
                sequence: 9,
                bytes: b"printf terminal\\n".to_vec(),
            },
            TerminalWebBinaryFrame::Output {
                sequence: 17,
                bytes: vec![0x1b, b'[', b'3', b'2', b'm'],
            },
            TerminalWebBinaryFrame::Snapshot {
                base_sequence: 4,
                next_sequence: 99,
                total_bytes: 16_384,
                bytes: b"bounded transcript tail".to_vec(),
            },
        ];

        for frame in frames {
            let encoded = encode_terminal_web_binary(&frame).expect("encode");
            assert_eq!(decode_terminal_web_binary(&encoded).expect("decode"), frame);
        }
    }

    #[test]
    fn input_frame_matches_the_browser_golden_vector() {
        let encoded = encode_terminal_web_binary(&TerminalWebBinaryFrame::Input {
            sequence: 42,
            bytes: vec![0, 27, 255],
        })
        .expect("encode");
        let hex = encoded
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        assert_eq!(
            hex,
            "4854575300010100000000000000002a0000000000000000000000000000000000000003001bff"
        );
    }

    #[test]
    fn declared_payload_length_must_match_the_websocket_message() {
        let mut encoded = encode_terminal_web_binary(&TerminalWebBinaryFrame::Output {
            sequence: 1,
            bytes: b"abc".to_vec(),
        })
        .expect("encode");
        encoded[35] = 2;

        assert_eq!(
            decode_terminal_web_binary(&encoded).expect_err("must reject mismatched payload"),
            TerminalWebError::PayloadLengthMismatch {
                declared: 2,
                actual: 3,
            }
        );
    }

    #[test]
    fn client_hello_cannot_choose_a_program_or_environment() {
        let hello = TerminalWebClientControl::Hello {
            protocol_version: TERMINAL_WEB_PROTOCOL_VERSION,
            attachment_id: None,
            after_sequence: 0,
            size: TerminalSize::default(),
            cwd: Some(PathBuf::from("/tmp/project")),
        };
        let value = serde_json::to_value(hello).expect("serialize");

        assert_eq!(value["type"], "hello");
        assert!(value.get("program").is_none());
        assert!(value.get("args").is_none());
        assert!(value.get("env").is_none());
    }

    #[test]
    fn oversized_payload_is_rejected_before_copying() {
        let frame = TerminalWebBinaryFrame::Input {
            sequence: 1,
            bytes: vec![0; MAX_TERMINAL_WEB_PAYLOAD_BYTES + 1],
        };
        assert!(matches!(
            encode_terminal_web_binary(&frame),
            Err(TerminalWebError::PayloadTooLarge { .. })
        ));
    }
}
