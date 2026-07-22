use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use hyper_term_protocol::{EVENT_SCHEMA_VERSION, EventEnvelope, EventId, NewEvent};
use thiserror::Error;

const MAX_EVENT_LINE_BYTES: usize = 1024 * 1024;

pub trait EventJournal {
    fn append(&mut self, event: NewEvent) -> Result<EventEnvelope, JournalError>;
    fn read_after(&self, sequence: u64) -> Vec<EventEnvelope>;
    fn last_sequence(&self) -> u64;
}

pub struct JsonlJournal {
    path: PathBuf,
    file: File,
    events: Vec<EventEnvelope>,
}

impl JsonlJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = open_private_journal(&path)?;
        let events = read_existing(&file)?;
        Ok(Self { path, file, events })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn all(&self) -> &[EventEnvelope] {
        &self.events
    }

    pub fn prepare(&self, event: NewEvent) -> Result<EventEnvelope, JournalError> {
        let sequence = self.last_sequence() + 1;
        let recorded_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| JournalError::ClockBeforeUnixEpoch)?
            .as_millis()
            .try_into()
            .map_err(|_| JournalError::TimestampOverflow)?;
        Ok(EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            sequence,
            event_id: EventId::new(),
            recorded_at_ms,
            task_id: event.task_id,
            run_id: event.run_id,
            operation_id: event.operation_id,
            causation_id: event.causation_id,
            correlation_id: event.correlation_id,
            payload: event.payload,
        })
    }

    pub fn append_envelope(
        &mut self,
        envelope: EventEnvelope,
    ) -> Result<EventEnvelope, JournalError> {
        envelope.validate().map_err(JournalError::InvalidEvent)?;
        let expected = self.last_sequence() + 1;
        if envelope.sequence != expected {
            return Err(JournalError::SequenceGap {
                expected,
                actual: envelope.sequence,
            });
        }
        let encoded = serde_json::to_vec(&envelope)?;
        if encoded.len() > MAX_EVENT_LINE_BYTES {
            return Err(JournalError::EventTooLarge(encoded.len()));
        }
        self.file.write_all(&encoded)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.file.sync_data()?;
        self.events.push(envelope.clone());
        Ok(envelope)
    }
}

impl EventJournal for JsonlJournal {
    fn append(&mut self, event: NewEvent) -> Result<EventEnvelope, JournalError> {
        let envelope = self.prepare(event)?;
        self.append_envelope(envelope)
    }

    fn read_after(&self, sequence: u64) -> Vec<EventEnvelope> {
        self.events
            .iter()
            .filter(|event| event.sequence > sequence)
            .cloned()
            .collect()
    }

    fn last_sequence(&self) -> u64 {
        self.events.last().map_or(0, |event| event.sequence)
    }
}

fn open_private_journal(path: &Path) -> Result<File, JournalError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(JournalError::UnsafePath(path.to_path_buf()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(JournalError::UnsafePath(path.to_path_buf()));
    }
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn read_existing(file: &File) -> Result<Vec<EventEnvelope>, JournalError> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut line = Vec::new();
    let mut expected_sequence = 1_u64;

    loop {
        line.clear();
        let bytes_read = reader.read_until(b'\n', &mut line)?;
        if bytes_read == 0 {
            break;
        }
        if bytes_read > MAX_EVENT_LINE_BYTES + 1 {
            return Err(JournalError::EventTooLarge(bytes_read));
        }
        if line.last() != Some(&b'\n') {
            return Err(JournalError::TornTail);
        }
        line.pop();
        if line.is_empty() {
            return Err(JournalError::BlankLine);
        }
        let event: EventEnvelope = serde_json::from_slice(&line)?;
        event.validate().map_err(JournalError::InvalidEvent)?;
        if event.sequence != expected_sequence {
            return Err(JournalError::SequenceGap {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        expected_sequence += 1;
        events.push(event);
    }
    Ok(events)
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal path is not a private regular file: {0}")]
    UnsafePath(PathBuf),
    #[error("journal JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid journal event: {0}")]
    InvalidEvent(&'static str),
    #[error("journal event is too large: {0} bytes")]
    EventTooLarge(usize),
    #[error("journal contains a blank line")]
    BlankLine,
    #[error("journal ends with a partial event")]
    TornTail,
    #[error("journal sequence gap: expected {expected}, got {actual}")]
    SequenceGap { expected: u64, actual: u64 },
    #[error("system clock is before Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("timestamp does not fit in u64 milliseconds")]
    TimestampOverflow,
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    #[cfg(unix)]
    use std::os::fd::AsRawFd;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    use hyper_term_protocol::{DomainEvent, NewEvent, TaskId};
    use tempfile::tempdir;

    use super::*;

    fn task_event(task_id: TaskId, title: &str) -> NewEvent {
        NewEvent {
            task_id,
            run_id: None,
            operation_id: None,
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::TaskCreated {
                title: title.into(),
            },
        }
    }

    #[test]
    fn journal_reopens_with_contiguous_sequence() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("events.jsonl");
        let task_id = TaskId::new();
        {
            let mut journal = JsonlJournal::open(&path).expect("open");
            assert_eq!(
                journal.append(task_event(task_id, "one")).unwrap().sequence,
                1
            );
            assert_eq!(
                journal.append(task_event(task_id, "two")).unwrap().sequence,
                2
            );
        }

        let journal = JsonlJournal::open(&path).expect("reopen");
        assert_eq!(journal.last_sequence(), 2);
        assert_eq!(journal.read_after(1).len(), 1);
    }

    #[test]
    fn torn_tail_is_rejected_instead_of_silently_discarded() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("events.jsonl");
        let mut file = File::create(&path).expect("create");
        file.write_all(b"{\"partial\":true}").expect("write");
        file.sync_all().expect("sync");

        let error = JsonlJournal::open(&path)
            .err()
            .expect("must reject torn tail");
        assert!(matches!(error, JournalError::TornTail));
    }

    #[test]
    #[cfg(unix)]
    fn journal_migrates_private_mode_and_is_not_inherited_by_exec() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("events.jsonl");
        File::create(&path).expect("journal fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("public fixture mode");

        let journal = JsonlJournal::open(&path).expect("private journal");
        assert_eq!(
            journal
                .file
                .metadata()
                .expect("journal metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let flags = unsafe { libc::fcntl(journal.file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    #[cfg(unix)]
    fn journal_rejects_a_symbolic_link_before_reading_it() {
        let directory = tempdir().expect("tempdir");
        let target = directory.path().join("outside.jsonl");
        File::create(&target).expect("journal target");
        let path = directory.path().join("events.jsonl");
        symlink(&target, &path).expect("journal symlink");

        assert!(matches!(
            JsonlJournal::open(&path),
            Err(JournalError::UnsafePath(rejected)) if rejected == path
        ));
    }
}
