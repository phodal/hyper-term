use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hyper_term_protocol::{
    ArtifactId, GenUiRuntimeTraceEvent, GenUiRuntimeTraceInput, GenUiRuntimeTraceKind,
    GenUiRuntimeTraceProjection, TaskId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const RUNTIME_TRACE_SCHEMA_VERSION: u16 = 1;
const MAX_TRACE_EVENTS_PER_APPEND: usize = 16;
const MAX_TRACE_EVENTS_PROJECTED: usize = 256;
const MAX_TRACE_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TRACE_EVENT_BYTES: usize = 32 * 1024;
const MAX_TRACE_NAME_BYTES: usize = 128;
const MAX_TRACE_VALUE_DEPTH: usize = 8;
const MAX_TRACE_VALUE_NODES: usize = 256;
const MAX_TRACE_STRING_BYTES: usize = 2048;
const REDACTED_VALUE: &str = "[REDACTED]";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredRuntimeTraceEvent {
    task_id: TaskId,
    #[serde(flatten)]
    event: GenUiRuntimeTraceEvent,
}

pub(crate) struct ArtifactRuntimeTraceStore {
    root: PathBuf,
}

impl ArtifactRuntimeTraceStore {
    pub(crate) fn open(state_directory: &Path) -> Result<Self, RuntimeTraceStoreError> {
        let root = state_directory.join("artifact-runtime-trace");
        create_private_directory(&root)?;
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    pub(crate) fn load(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
        source_revision: u64,
    ) -> Result<GenUiRuntimeTraceProjection, RuntimeTraceStoreError> {
        let events = self.read_events(task_id, artifact_id, source_revision)?;
        let keep_from = events.len().saturating_sub(MAX_TRACE_EVENTS_PROJECTED);
        let events = events
            .into_iter()
            .skip(keep_from)
            .map(|stored| stored.event)
            .collect::<Vec<_>>();
        let projection_digest = replay_projection_digest(source_revision, &events)?;
        Ok(GenUiRuntimeTraceProjection {
            artifact_id,
            source_revision,
            projection_digest,
            events,
        })
    }

    pub(crate) fn append(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
        source_revision: u64,
        inputs: Vec<GenUiRuntimeTraceInput>,
    ) -> Result<GenUiRuntimeTraceProjection, RuntimeTraceStoreError> {
        if source_revision == 0 || inputs.is_empty() || inputs.len() > MAX_TRACE_EVENTS_PER_APPEND {
            return Err(RuntimeTraceStoreError::InvalidEvent);
        }
        let existing = self.read_events(task_id, artifact_id, source_revision)?;
        let mut by_client = HashMap::new();
        let mut stream_tails = HashMap::new();
        for stored in &existing {
            by_client.insert(
                (stored.event.stream_id, stored.event.client_sequence),
                stored.event.payload_digest.clone(),
            );
            stream_tails
                .entry(stored.event.stream_id)
                .and_modify(|tail: &mut u64| *tail = (*tail).max(stored.event.client_sequence))
                .or_insert(stored.event.client_sequence);
        }

        let mut event_sequence = existing
            .last()
            .map(|stored| stored.event.event_sequence)
            .unwrap_or(0);
        let mut accepted = Vec::new();
        for input in inputs {
            let prepared = prepare_input(input)?;
            let key = (prepared.stream_id, prepared.client_sequence);
            if let Some(existing_digest) = by_client.get(&key) {
                if existing_digest == &prepared.payload_digest {
                    continue;
                }
                return Err(RuntimeTraceStoreError::SequenceConflict);
            }
            let expected = stream_tails
                .get(&prepared.stream_id)
                .copied()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(RuntimeTraceStoreError::SequenceOverflow)?;
            if prepared.client_sequence != expected {
                return Err(RuntimeTraceStoreError::SequenceGap {
                    expected,
                    actual: prepared.client_sequence,
                });
            }
            event_sequence = event_sequence
                .checked_add(1)
                .ok_or(RuntimeTraceStoreError::SequenceOverflow)?;
            let event = GenUiRuntimeTraceEvent {
                schema_version: RUNTIME_TRACE_SCHEMA_VERSION,
                event_sequence,
                artifact_id,
                source_revision,
                stream_id: prepared.stream_id,
                client_sequence: prepared.client_sequence,
                kind: prepared.kind,
                name: prepared.name,
                payload: prepared.payload,
                payload_digest: prepared.payload_digest.clone(),
                redacted: prepared.redacted,
                recorded_at_ms: now_ms()?,
            };
            let stored = StoredRuntimeTraceEvent { task_id, event };
            by_client.insert(key, prepared.payload_digest);
            stream_tails.insert(prepared.stream_id, prepared.client_sequence);
            accepted.push(stored);
        }
        if !accepted.is_empty() {
            self.append_events(task_id, artifact_id, &accepted)?;
        }
        self.load(task_id, artifact_id, source_revision)
    }

    fn read_events(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
        source_revision: u64,
    ) -> Result<Vec<StoredRuntimeTraceEvent>, RuntimeTraceStoreError> {
        let task_root = self.task_root(task_id);
        if !task_root.exists() {
            return Ok(Vec::new());
        }
        validate_private_directory(&task_root)?;
        let path = self.journal_path(task_id, artifact_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let encoded = read_bounded_file(&path, MAX_TRACE_JOURNAL_BYTES)?;
        if !encoded.is_empty() && !encoded.ends_with(b"\n") {
            return Err(RuntimeTraceStoreError::TornJournal);
        }
        let mut events = Vec::new();
        let mut last_sequence = 0;
        for line in encoded
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if line.len() > MAX_TRACE_EVENT_BYTES {
                return Err(RuntimeTraceStoreError::TooLarge);
            }
            let stored: StoredRuntimeTraceEvent = serde_json::from_slice(line)?;
            let expected_digest = trace_digest(
                stored.event.stream_id,
                stored.event.client_sequence,
                stored.event.kind,
                &stored.event.name,
                &stored.event.payload,
            )?;
            if stored.task_id != task_id
                || stored.event.schema_version != RUNTIME_TRACE_SCHEMA_VERSION
                || stored.event.artifact_id != artifact_id
                || stored.event.source_revision != source_revision
                || stored.event.event_sequence != last_sequence + 1
                || stored.event.client_sequence == 0
                || !is_sha256(&stored.event.payload_digest)
                || stored.event.payload_digest != expected_digest
            {
                return Err(RuntimeTraceStoreError::ContextMismatch);
            }
            last_sequence = stored.event.event_sequence;
            events.push(stored);
        }
        Ok(events)
    }

    fn append_events(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
        events: &[StoredRuntimeTraceEvent],
    ) -> Result<(), RuntimeTraceStoreError> {
        let task_root = self.ensure_task_root(task_id)?;
        let path = self.journal_path(task_id, artifact_id);
        let current_bytes = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let mut encoded = Vec::new();
        for event in events {
            let mut line = serde_json::to_vec(event)?;
            if line.len() > MAX_TRACE_EVENT_BYTES {
                return Err(RuntimeTraceStoreError::TooLarge);
            }
            line.push(b'\n');
            encoded.extend(line);
        }
        if current_bytes.saturating_add(encoded.len() as u64) > MAX_TRACE_JOURNAL_BYTES {
            return Err(RuntimeTraceStoreError::TooLarge);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        File::open(task_root)?.sync_all()?;
        Ok(())
    }

    fn task_root(&self, task_id: TaskId) -> PathBuf {
        self.root.join(task_id.to_string())
    }

    fn ensure_task_root(&self, task_id: TaskId) -> Result<PathBuf, RuntimeTraceStoreError> {
        let task_root = self.task_root(task_id);
        create_private_directory(&task_root)?;
        File::open(&self.root)?.sync_all()?;
        Ok(task_root)
    }

    fn journal_path(&self, task_id: TaskId, artifact_id: ArtifactId) -> PathBuf {
        self.task_root(task_id).join(format!("{artifact_id}.jsonl"))
    }
}

struct PreparedInput {
    stream_id: Uuid,
    client_sequence: u64,
    kind: hyper_term_protocol::GenUiRuntimeTraceKind,
    name: String,
    payload: Value,
    payload_digest: String,
    redacted: bool,
}

fn prepare_input(input: GenUiRuntimeTraceInput) -> Result<PreparedInput, RuntimeTraceStoreError> {
    if input.schema_version != RUNTIME_TRACE_SCHEMA_VERSION
        || input.stream_id.is_nil()
        || input.client_sequence == 0
        || input.name.is_empty()
        || input.name.len() > MAX_TRACE_NAME_BYTES
        || input.name.contains(['\0', '\n', '\r'])
    {
        return Err(RuntimeTraceStoreError::InvalidEvent);
    }
    if input.kind == GenUiRuntimeTraceKind::EffectReceipt {
        validate_effect_receipt(&input.payload)?;
    }
    let mut nodes = 0;
    let mut redacted = false;
    let payload = sanitize_value(input.payload, 0, &mut nodes, &mut redacted)?;
    let payload_digest = trace_digest(
        input.stream_id,
        input.client_sequence,
        input.kind,
        &input.name,
        &payload,
    )?;
    let canonical = serde_json::to_vec(&(
        RUNTIME_TRACE_SCHEMA_VERSION,
        input.stream_id,
        input.client_sequence,
        input.kind,
        &input.name,
        &payload,
    ))?;
    if canonical.len() > MAX_TRACE_EVENT_BYTES {
        return Err(RuntimeTraceStoreError::TooLarge);
    }
    Ok(PreparedInput {
        stream_id: input.stream_id,
        client_sequence: input.client_sequence,
        kind: input.kind,
        name: input.name,
        payload,
        payload_digest,
        redacted,
    })
}

fn validate_effect_receipt(payload: &Value) -> Result<(), RuntimeTraceStoreError> {
    let object = payload
        .as_object()
        .ok_or(RuntimeTraceStoreError::InvalidEvent)?;
    if !object.contains_key("input")
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "input" | "outcome" | "output" | "error"))
    {
        return Err(RuntimeTraceStoreError::InvalidEvent);
    }
    match object.get("outcome").and_then(Value::as_str) {
        Some("succeeded") if object.contains_key("output") && !object.contains_key("error") => {}
        Some("failed")
            if object.get("error").is_some_and(Value::is_string)
                && !object.contains_key("output") => {}
        _ => return Err(RuntimeTraceStoreError::InvalidEvent),
    }
    Ok(())
}

fn trace_digest(
    stream_id: Uuid,
    client_sequence: u64,
    kind: hyper_term_protocol::GenUiRuntimeTraceKind,
    name: &str,
    payload: &Value,
) -> Result<String, RuntimeTraceStoreError> {
    let canonical = serde_json::to_vec(&(
        RUNTIME_TRACE_SCHEMA_VERSION,
        stream_id,
        client_sequence,
        kind,
        name,
        payload,
    ))?;
    Ok(hex_digest(Sha256::digest(canonical)))
}

fn replay_projection_digest(
    source_revision: u64,
    events: &[GenUiRuntimeTraceEvent],
) -> Result<String, RuntimeTraceStoreError> {
    let deterministic = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                GenUiRuntimeTraceKind::Action
                    | GenUiRuntimeTraceKind::Checkpoint
                    | GenUiRuntimeTraceKind::EffectReceipt
            )
        })
        .map(|event| {
            (
                event.event_sequence,
                event.stream_id,
                event.client_sequence,
                event.kind,
                &event.name,
                &event.payload_digest,
                event.redacted,
            )
        })
        .collect::<Vec<_>>();
    let canonical =
        serde_json::to_vec(&(RUNTIME_TRACE_SCHEMA_VERSION, source_revision, deterministic))?;
    Ok(hex_digest(Sha256::digest(canonical)))
}

fn sanitize_value(
    value: Value,
    depth: usize,
    nodes: &mut usize,
    redacted: &mut bool,
) -> Result<Value, RuntimeTraceStoreError> {
    *nodes = nodes.saturating_add(1);
    if depth > MAX_TRACE_VALUE_DEPTH || *nodes > MAX_TRACE_VALUE_NODES {
        return Err(RuntimeTraceStoreError::TooLarge);
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value),
        Value::String(value) => {
            if value.len() > MAX_TRACE_STRING_BYTES {
                return Err(RuntimeTraceStoreError::TooLarge);
            }
            Ok(Value::String(value))
        }
        Value::Array(values) => values
            .into_iter()
            .map(|value| sanitize_value(value, depth + 1, nodes, redacted))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in values {
                if key.is_empty() || key.len() > MAX_TRACE_NAME_BYTES {
                    return Err(RuntimeTraceStoreError::InvalidEvent);
                }
                if sensitive_key(&key) {
                    *redacted = true;
                    sorted.insert(key, Value::String(REDACTED_VALUE.into()));
                } else {
                    sorted.insert(key, sanitize_value(value, depth + 1, nodes, redacted)?);
                }
            }
            Ok(Value::Object(sorted.into_iter().collect::<Map<_, _>>()))
        }
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    [
        "authorization",
        "cookie",
        "credential",
        "password",
        "secret",
        "token",
        "api_key",
        "private_key",
        "environment",
        "env",
    ]
    .iter()
    .any(|candidate| normalized == *candidate || normalized.ends_with(&format!("_{candidate}")))
}

fn now_ms() -> Result<u64, RuntimeTraceStoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeTraceStoreError::Clock)?
        .as_millis()
        .try_into()
        .map_err(|_| RuntimeTraceStoreError::Clock)
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, RuntimeTraceStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(RuntimeTraceStoreError::InvalidPath);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(RuntimeTraceStoreError::TooLarge);
    }
    Ok(bytes)
}

fn create_private_directory(path: &Path) -> Result<(), RuntimeTraceStoreError> {
    if path.exists() {
        validate_private_directory(path)?;
    } else {
        fs::create_dir(path)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), RuntimeTraceStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RuntimeTraceStoreError::InvalidPath);
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeTraceStoreError {
    #[error("runtime trace path is invalid")]
    InvalidPath,
    #[error("runtime trace event is invalid")]
    InvalidEvent,
    #[error("runtime trace context does not match the current artifact")]
    ContextMismatch,
    #[error("runtime trace journal has a torn tail")]
    TornJournal,
    #[error("runtime trace exceeds its bound")]
    TooLarge,
    #[error("runtime trace sequence conflicts with persisted evidence")]
    SequenceConflict,
    #[error("runtime trace sequence has a gap: expected {expected}, got {actual}")]
    SequenceGap { expected: u64, actual: u64 },
    #[error("runtime trace sequence overflowed")]
    SequenceOverflow,
    #[error("runtime trace clock failed")]
    Clock,
    #[error("runtime trace I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("runtime trace JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper_term_protocol::GenUiRuntimeTraceKind;

    fn input(stream_id: Uuid, client_sequence: u64, payload: Value) -> GenUiRuntimeTraceInput {
        GenUiRuntimeTraceInput {
            schema_version: 1,
            stream_id,
            client_sequence,
            kind: GenUiRuntimeTraceKind::Checkpoint,
            name: "counter.changed".into(),
            payload,
        }
    }

    #[test]
    fn runtime_trace_reopens_redacted_ordered_checkpoints() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactRuntimeTraceStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let stream_id = Uuid::new_v4();
        let first = store
            .append(
                task_id,
                artifact_id,
                7,
                vec![
                    input(stream_id, 1, serde_json::json!({"count": 1})),
                    input(
                        stream_id,
                        2,
                        serde_json::json!({"count": 2, "api_token": "do-not-store"}),
                    ),
                ],
            )
            .unwrap();
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.events[1].payload["api_token"], REDACTED_VALUE);
        assert!(first.events[1].redacted);
        assert!(is_sha256(&first.events[1].payload_digest));

        let reopened = ArtifactRuntimeTraceStore::open(temporary.path())
            .unwrap()
            .load(task_id, artifact_id, 7)
            .unwrap();
        assert_eq!(reopened, first);
    }

    #[test]
    fn runtime_trace_retry_is_idempotent_and_conflicts_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactRuntimeTraceStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let stream_id = Uuid::new_v4();
        let event = input(stream_id, 1, serde_json::json!({"expanded": true}));
        store
            .append(task_id, artifact_id, 3, vec![event.clone()])
            .unwrap();
        assert_eq!(
            store
                .append(task_id, artifact_id, 3, vec![event])
                .unwrap()
                .events
                .len(),
            1
        );
        assert!(matches!(
            store.append(
                task_id,
                artifact_id,
                3,
                vec![input(stream_id, 1, serde_json::json!({"expanded": false}))]
            ),
            Err(RuntimeTraceStoreError::SequenceConflict)
        ));
        assert!(matches!(
            store.append(
                task_id,
                artifact_id,
                3,
                vec![input(stream_id, 3, serde_json::json!({"expanded": false}))]
            ),
            Err(RuntimeTraceStoreError::SequenceGap { .. })
        ));
    }

    #[test]
    fn runtime_trace_rejects_oversized_or_torn_evidence() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactRuntimeTraceStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let stream_id = Uuid::new_v4();
        assert!(matches!(
            store.append(
                task_id,
                artifact_id,
                1,
                vec![input(
                    stream_id,
                    1,
                    Value::String("x".repeat(MAX_TRACE_STRING_BYTES + 1))
                )]
            ),
            Err(RuntimeTraceStoreError::TooLarge)
        ));

        let task_root = store.ensure_task_root(task_id).unwrap();
        fs::write(store.journal_path(task_id, artifact_id), b"{\"torn\":true}").unwrap();
        assert!(matches!(
            store.load(task_id, artifact_id, 1),
            Err(RuntimeTraceStoreError::TornJournal)
        ));
        assert!(task_root.is_dir());
    }

    #[test]
    fn runtime_trace_recomputes_integrity_digests_when_reopening() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactRuntimeTraceStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        store
            .append(
                task_id,
                artifact_id,
                4,
                vec![input(Uuid::new_v4(), 1, serde_json::json!({"count": 1}))],
            )
            .unwrap();
        let path = store.journal_path(task_id, artifact_id);
        let encoded = fs::read_to_string(&path).unwrap();
        fs::write(path, encoded.replace("\"count\":1", "\"count\":2")).unwrap();
        assert!(matches!(
            store.load(task_id, artifact_id, 4),
            Err(RuntimeTraceStoreError::ContextMismatch)
        ));
    }

    #[test]
    fn runtime_trace_validates_receipts_and_replays_to_a_stable_projection_digest() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactRuntimeTraceStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let stream_id = Uuid::new_v4();
        let mut receipt = input(
            stream_id,
            1,
            serde_json::json!({
                "input": {"city": "Shanghai"},
                "outcome": "succeeded",
                "output": {"temperature": 31}
            }),
        );
        receipt.kind = GenUiRuntimeTraceKind::EffectReceipt;
        receipt.name = "weather.lookup".into();
        let first = store
            .append(task_id, artifact_id, 8, vec![receipt])
            .unwrap();
        assert!(is_sha256(&first.projection_digest));
        let mut observation = input(
            stream_id,
            2,
            serde_json::json!({"message": "layout-only observation"}),
        );
        observation.kind = GenUiRuntimeTraceKind::Console;
        observation.name = "preview.debug".into();
        let with_observation = store
            .append(task_id, artifact_id, 8, vec![observation])
            .unwrap();
        assert_eq!(with_observation.projection_digest, first.projection_digest);
        assert_eq!(
            ArtifactRuntimeTraceStore::open(temporary.path())
                .unwrap()
                .load(task_id, artifact_id, 8)
                .unwrap()
                .projection_digest,
            first.projection_digest
        );

        let mut malformed = input(
            Uuid::new_v4(),
            1,
            serde_json::json!({
                "input": {},
                "outcome": "succeeded",
                "error": "ambiguous receipt"
            }),
        );
        malformed.kind = GenUiRuntimeTraceKind::EffectReceipt;
        assert!(matches!(
            store.append(task_id, ArtifactId::new(), 8, vec![malformed]),
            Err(RuntimeTraceStoreError::InvalidEvent)
        ));
    }
}
