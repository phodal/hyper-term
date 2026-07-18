use std::io::{BufRead, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::DriverError;

pub const DEFAULT_MAX_DRIVER_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverFraming {
    ContentLength,
    JsonLines,
}

impl DriverFraming {
    pub fn read<R: BufRead>(
        self,
        reader: &mut R,
        max_bytes: usize,
    ) -> Result<Option<Value>, DriverError> {
        match self {
            Self::ContentLength => read_content_length(reader, max_bytes),
            Self::JsonLines => read_json_line(reader, max_bytes),
        }
    }

    pub fn write<W: Write>(
        self,
        writer: &mut W,
        value: &Value,
        max_bytes: usize,
    ) -> Result<(), DriverError> {
        let payload = serde_json::to_vec(value)?;
        if payload.len() > max_bytes {
            return Err(DriverError::FrameTooLarge {
                size: payload.len(),
                maximum: max_bytes,
            });
        }
        match self {
            Self::ContentLength => {
                write!(writer, "Content-Length: {}\r\n\r\n", payload.len())?;
                writer.write_all(&payload)?;
            }
            Self::JsonLines => {
                writer.write_all(&payload)?;
                writer.write_all(b"\n")?;
            }
        }
        writer.flush()?;
        Ok(())
    }
}

fn read_content_length<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<Value>, DriverError> {
    let mut content_length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = Vec::new();
        let read = read_bounded_line(reader, &mut line, MAX_HEADER_BYTES)?;
        if read == 0 {
            if header_bytes == 0 {
                return Ok(None);
            }
            return Err(DriverError::InvalidFrame(
                "content-length header ended before its blank line".into(),
            ));
        }
        header_bytes += read;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(DriverError::InvalidFrame(
                "content-length headers exceed the configured bound".into(),
            ));
        }
        if line == b"\n" || line == b"\r\n" {
            break;
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| DriverError::InvalidFrame("header is not UTF-8".into()))?;
        let Some((name, value)) = line.trim_end().split_once(':') else {
            return Err(DriverError::InvalidFrame(
                "malformed content-length header".into(),
            ));
        };
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(DriverError::InvalidFrame(
                    "duplicate content-length header".into(),
                ));
            }
            content_length =
                Some(value.trim().parse::<usize>().map_err(|_| {
                    DriverError::InvalidFrame("invalid content-length value".into())
                })?);
        }
    }

    let length = content_length
        .ok_or_else(|| DriverError::InvalidFrame("missing content-length header".into()))?;
    if length > max_bytes {
        return Err(DriverError::FrameTooLarge {
            size: length,
            maximum: max_bytes,
        });
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

fn read_json_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<Value>, DriverError> {
    let mut line = Vec::new();
    let read = read_bounded_line(reader, &mut line, max_bytes + 2)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    if line.len() > max_bytes {
        return Err(DriverError::FrameTooLarge {
            size: line.len(),
            maximum: max_bytes,
        });
    }
    if line.is_empty() {
        return Err(DriverError::InvalidFrame("empty JSON line".into()));
    }
    Ok(Some(serde_json::from_slice(&line)?))
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    output: &mut Vec<u8>,
    limit: usize,
) -> Result<usize, DriverError> {
    let mut bounded = reader.take((limit + 1) as u64);
    let read = bounded.read_until(b'\n', output)?;
    if output.len() > limit {
        return Err(DriverError::InvalidFrame(
            "protocol line exceeds the configured bound".into(),
        ));
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::*;

    #[test]
    fn content_length_round_trips_json_rpc() {
        let value = json!({"jsonrpc": "2.0", "id": 7, "method": "initialize"});
        let mut bytes = Vec::new();
        DriverFraming::ContentLength
            .write(&mut bytes, &value, 1024)
            .unwrap();
        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(
            DriverFraming::ContentLength
                .read(&mut reader, 1024)
                .unwrap(),
            Some(value)
        );
    }

    #[test]
    fn content_length_rejects_oversized_payload_before_reading_it() {
        let mut reader = BufReader::new(Cursor::new(b"Content-Length: 4096\r\n\r\n{}".to_vec()));
        assert!(matches!(
            DriverFraming::ContentLength.read(&mut reader, 64),
            Err(DriverError::FrameTooLarge {
                size: 4096,
                maximum: 64
            })
        ));
    }

    #[test]
    fn content_length_rejects_duplicate_length() {
        let mut reader = BufReader::new(Cursor::new(
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        ));
        assert!(matches!(
            DriverFraming::ContentLength.read(&mut reader, 64),
            Err(DriverError::InvalidFrame(_))
        ));
    }

    #[test]
    fn json_lines_round_trip() {
        let value = json!({"type": "result", "ok": true});
        let mut bytes = Vec::new();
        DriverFraming::JsonLines
            .write(&mut bytes, &value, 1024)
            .unwrap();
        let mut reader = BufReader::new(Cursor::new(bytes));
        assert_eq!(
            DriverFraming::JsonLines.read(&mut reader, 1024).unwrap(),
            Some(value)
        );
    }
}
