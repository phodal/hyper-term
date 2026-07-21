use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hyper_term_protocol::TaskId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const AGENT_SESSION_BINDING_VERSION: u16 = 1;
const MAX_AGENT_SESSION_BINDINGS: usize = 8;
const MAX_AGENT_SESSION_BINDING_BYTES: usize = 4 * 1024;
pub(crate) const AGENT_SESSION_BINDING_FILE: &str = "agent-session-bindings.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct AgentSessionBindingSnapshot {
    version: u16,
    entries: Vec<AgentSessionBinding>,
}

impl Default for AgentSessionBindingSnapshot {
    fn default() -> Self {
        Self {
            version: AGENT_SESSION_BINDING_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct AgentSessionBinding {
    session_id: u16,
    provider: String,
    task_id: TaskId,
}

pub(crate) struct AgentSessionBindingStore {
    path: PathBuf,
    snapshot: Mutex<AgentSessionBindingSnapshot>,
}

impl AgentSessionBindingStore {
    pub(crate) fn open(state_directory: &Path) -> Result<Self, AgentSessionBindingStoreError> {
        let path = state_directory.join(AGENT_SESSION_BINDING_FILE);
        let snapshot = match load(&path)? {
            LoadResult::Snapshot(snapshot) => snapshot,
            LoadResult::Missing => {
                let snapshot = AgentSessionBindingSnapshot::default();
                replace(&path, &snapshot)?;
                snapshot
            }
            LoadResult::Invalid => {
                eprintln!(
                    "hyper-term: invalid Agent session binding state at {}; starting with no restored Agent history",
                    path.display()
                );
                let snapshot = AgentSessionBindingSnapshot::default();
                replace(&path, &snapshot)?;
                snapshot
            }
        };
        Ok(Self {
            path,
            snapshot: Mutex::new(snapshot),
        })
    }

    pub(crate) fn task_for(
        &self,
        session_id: u16,
        provider: &str,
    ) -> Result<Option<TaskId>, AgentSessionBindingStoreError> {
        let snapshot = self
            .snapshot
            .lock()
            .map_err(|_| AgentSessionBindingStoreError::Unavailable)?;
        Ok(snapshot
            .entries
            .iter()
            .find(|entry| entry.session_id == session_id && entry.provider == provider)
            .map(|entry| entry.task_id))
    }

    pub(crate) fn bind(
        &self,
        session_id: u16,
        provider: &str,
        task_id: TaskId,
    ) -> Result<(), AgentSessionBindingStoreError> {
        if !valid_session_id(session_id) || !valid_provider(provider) {
            return Err(AgentSessionBindingStoreError::Invalid);
        }
        let mut current = self
            .snapshot
            .lock()
            .map_err(|_| AgentSessionBindingStoreError::Unavailable)?;
        let mut next = current.clone();
        next.entries
            .retain(|entry| entry.session_id != session_id && entry.task_id != task_id);
        next.entries.push(AgentSessionBinding {
            session_id,
            provider: provider.to_owned(),
            task_id,
        });
        next.entries.sort_by_key(|entry| entry.session_id);
        validate(&next)?;
        replace(&self.path, &next)?;
        *current = next;
        Ok(())
    }

    pub(crate) fn forget(&self, session_id: u16) -> Result<(), AgentSessionBindingStoreError> {
        let mut current = self
            .snapshot
            .lock()
            .map_err(|_| AgentSessionBindingStoreError::Unavailable)?;
        let mut next = current.clone();
        next.entries.retain(|entry| entry.session_id != session_id);
        if next == *current {
            return Ok(());
        }
        replace(&self.path, &next)?;
        *current = next;
        Ok(())
    }
}

enum LoadResult {
    Missing,
    Invalid,
    Snapshot(AgentSessionBindingSnapshot),
}

fn load(path: &Path) -> Result<LoadResult, AgentSessionBindingStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadResult::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_AGENT_SESSION_BINDING_BYTES as u64
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Ok(LoadResult::Invalid);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_AGENT_SESSION_BINDING_BYTES as u64 + 1)
        .read_to_end(&mut encoded)?;
    if encoded.len() > MAX_AGENT_SESSION_BINDING_BYTES {
        return Ok(LoadResult::Invalid);
    }
    let Ok(snapshot) = serde_json::from_slice::<AgentSessionBindingSnapshot>(&encoded) else {
        return Ok(LoadResult::Invalid);
    };
    if validate(&snapshot).is_err() {
        return Ok(LoadResult::Invalid);
    }
    Ok(LoadResult::Snapshot(snapshot))
}

fn replace(
    path: &Path,
    snapshot: &AgentSessionBindingSnapshot,
) -> Result<(), AgentSessionBindingStoreError> {
    validate(snapshot)?;
    let parent = path
        .parent()
        .ok_or(AgentSessionBindingStoreError::Invalid)?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AgentSessionBindingStoreError::Invalid);
    }
    let mut encoded = serde_json::to_vec(snapshot)?;
    encoded.push(b'\n');
    if encoded.len() > MAX_AGENT_SESSION_BINDING_BYTES {
        return Err(AgentSessionBindingStoreError::TooLarge);
    }
    let temporary = parent.join(format!(".agent-session-bindings-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}

fn validate(snapshot: &AgentSessionBindingSnapshot) -> Result<(), AgentSessionBindingStoreError> {
    if snapshot.version != AGENT_SESSION_BINDING_VERSION
        || snapshot.entries.len() > MAX_AGENT_SESSION_BINDINGS
    {
        return Err(AgentSessionBindingStoreError::Invalid);
    }
    for (index, entry) in snapshot.entries.iter().enumerate() {
        if !valid_session_id(entry.session_id)
            || !valid_provider(&entry.provider)
            || snapshot.entries[..index]
                .iter()
                .any(|prior| prior.session_id == entry.session_id || prior.task_id == entry.task_id)
        {
            return Err(AgentSessionBindingStoreError::Invalid);
        }
    }
    Ok(())
}

fn valid_session_id(session_id: u16) -> bool {
    (1..=999).contains(&session_id)
}

fn valid_provider(provider: &str) -> bool {
    !provider.is_empty()
        && provider.len() <= 64
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Debug, Error)]
pub(crate) enum AgentSessionBindingStoreError {
    #[error("Agent session binding state is invalid")]
    Invalid,
    #[error("Agent session binding state is unavailable")]
    Unavailable,
    #[error("Agent session binding state exceeds its byte budget")]
    TooLarge,
    #[error("Agent session binding I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Agent session binding JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_are_private_atomic_and_reopenable() {
        let temporary = tempfile::tempdir().unwrap();
        let store = AgentSessionBindingStore::open(temporary.path()).unwrap();
        let first = TaskId::new();
        let second = TaskId::new();
        store.bind(7, "codex-acp", first).unwrap();
        store.bind(3, "claude-acp", second).unwrap();

        let path = temporary.path().join(AGENT_SESSION_BINDING_FILE);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let encoded = fs::read_to_string(&path).unwrap();
        assert!(!encoded.contains("prompt"));
        assert!(!encoded.contains("token"));

        let reopened = AgentSessionBindingStore::open(temporary.path()).unwrap();
        assert_eq!(reopened.task_for(7, "codex-acp").unwrap(), Some(first));
        assert_eq!(reopened.task_for(3, "claude-acp").unwrap(), Some(second));
        reopened.forget(7).unwrap();
        assert_eq!(reopened.task_for(7, "codex-acp").unwrap(), None);
        assert_eq!(
            AgentSessionBindingStore::open(temporary.path())
                .unwrap()
                .task_for(3, "claude-acp")
                .unwrap(),
            Some(second)
        );
    }

    #[test]
    fn invalid_state_degrades_to_an_empty_private_store() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join(AGENT_SESSION_BINDING_FILE);
        fs::write(&path, b"{\"version\":1").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        let store = AgentSessionBindingStore::open(temporary.path()).unwrap();
        assert_eq!(store.task_for(4, "codex").unwrap(), None);
        assert_eq!(
            serde_json::from_slice::<AgentSessionBindingSnapshot>(&fs::read(&path).unwrap())
                .unwrap(),
            AgentSessionBindingSnapshot::default()
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
