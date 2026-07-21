use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hyper_term_protocol::{
    ArtifactId, MAX_GENUI_SOURCE_BYTES, MAX_GENUI_SOURCE_FILES, MAX_GENUI_VIRTUAL_PATH_BYTES,
    TaskId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const LEGACY_EDITOR_STATE_SCHEMA_VERSION: u32 = 1;
const EDITOR_STATE_SCHEMA_VERSION: u32 = 2;
const MAX_EDITOR_FILES: usize = MAX_GENUI_SOURCE_FILES;
const MAX_EDITOR_SOURCE_BYTES: usize = MAX_GENUI_SOURCE_BYTES;
const MAX_EDITOR_SELECTIONS: usize = 100;
const MAX_EDITOR_STATE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_EDITOR_JOURNAL_BYTES: u64 = 10 * 1024 * 1024;
const COMPACT_EVERY_REVISIONS: u64 = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactEditorView {
    Code,
    Diff,
    Trace,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ArtifactEditorSelection {
    pub anchor: u32,
    pub head: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ArtifactEditorCheckpointRequest {
    pub expected_revision: u64,
    pub base_source_revision: u64,
    pub files: BTreeMap<String, String>,
    pub active_path: String,
    pub view: ArtifactEditorView,
    #[serde(default)]
    pub selections: BTreeMap<String, ArtifactEditorSelection>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ArtifactEditorCheckpoint {
    pub schema_version: u32,
    pub artifact_id: ArtifactId,
    pub base_source_revision: u64,
    pub revision: u64,
    pub state_digest: String,
    pub entrypoint: String,
    pub files: BTreeMap<String, String>,
    pub active_path: String,
    pub view: ArtifactEditorView,
    pub selections: BTreeMap<String, ArtifactEditorSelection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredEditorCheckpoint {
    schema_version: u32,
    task_id: TaskId,
    artifact_id: ArtifactId,
    base_source_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    baseline_digest: Option<String>,
    revision: u64,
    entrypoint: String,
    files: BTreeMap<String, String>,
    active_path: String,
    view: ArtifactEditorView,
    selections: BTreeMap<String, ArtifactEditorSelection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EditorTransaction {
    schema_version: u32,
    task_id: TaskId,
    artifact_id: ArtifactId,
    base_source_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    baseline_digest: Option<String>,
    revision: u64,
    changed_files: BTreeMap<String, String>,
    active_path: String,
    view: ArtifactEditorView,
    selections: BTreeMap<String, ArtifactEditorSelection>,
}

pub(crate) struct ArtifactEditorStore {
    root: PathBuf,
}

impl ArtifactEditorStore {
    pub(crate) fn open(state_directory: &Path) -> Result<Self, ArtifactEditorStoreError> {
        let root = state_directory.join("artifact-editor");
        create_private_directory(&root)?;
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    pub(crate) fn load(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
        base_source_revision: u64,
        entrypoint: &str,
        baseline_files: &BTreeMap<String, String>,
    ) -> Result<ArtifactEditorCheckpoint, ArtifactEditorStoreError> {
        validate_editor_state(entrypoint, baseline_files, entrypoint, &BTreeMap::new())?;
        let baseline_digest =
            editor_baseline_digest(base_source_revision, entrypoint, baseline_files)?;
        let task_root = self.task_root(task_id);
        if !task_root.exists() {
            return checkpoint_from_stored(StoredEditorCheckpoint {
                schema_version: EDITOR_STATE_SCHEMA_VERSION,
                task_id,
                artifact_id,
                base_source_revision,
                baseline_digest: Some(baseline_digest),
                revision: 0,
                entrypoint: entrypoint.to_owned(),
                files: baseline_files.clone(),
                active_path: entrypoint.to_owned(),
                view: ArtifactEditorView::Code,
                selections: BTreeMap::new(),
            });
        }
        validate_private_directory(&task_root)?;
        let snapshot_path = self.snapshot_path(task_id, artifact_id);
        let journal_path = self.journal_path(task_id, artifact_id);
        let mut migration_required = false;
        let mut state = if snapshot_path.exists() {
            let encoded = read_bounded_file(&snapshot_path, MAX_EDITOR_STATE_BYTES)?;
            let mut snapshot: StoredEditorCheckpoint = serde_json::from_slice(&encoded)?;
            migration_required |= migrate_stored_checkpoint(&mut snapshot, &baseline_digest)?;
            validate_stored_context(
                &snapshot,
                task_id,
                artifact_id,
                base_source_revision,
                &baseline_digest,
                entrypoint,
                baseline_files,
            )?;
            snapshot
        } else {
            StoredEditorCheckpoint {
                schema_version: EDITOR_STATE_SCHEMA_VERSION,
                task_id,
                artifact_id,
                base_source_revision,
                baseline_digest: Some(baseline_digest.clone()),
                revision: 0,
                entrypoint: entrypoint.to_owned(),
                files: baseline_files.clone(),
                active_path: entrypoint.to_owned(),
                view: ArtifactEditorView::Code,
                selections: BTreeMap::new(),
            }
        };
        if journal_path.exists() {
            let encoded = read_bounded_file(&journal_path, MAX_EDITOR_JOURNAL_BYTES)?;
            if !encoded.is_empty() && !encoded.ends_with(b"\n") {
                return Err(ArtifactEditorStoreError::TornJournal);
            }
            for line in encoded
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                if line.len() as u64 > MAX_EDITOR_STATE_BYTES {
                    return Err(ArtifactEditorStoreError::TooLarge);
                }
                let mut transaction: EditorTransaction = serde_json::from_slice(line)?;
                migration_required |= migrate_transaction(&mut transaction, &baseline_digest)?;
                validate_transaction_context(
                    &transaction,
                    task_id,
                    artifact_id,
                    base_source_revision,
                    &baseline_digest,
                )?;
                if transaction.revision <= state.revision {
                    continue;
                }
                if transaction.revision != state.revision + 1 {
                    return Err(ArtifactEditorStoreError::RevisionGap);
                }
                for (path, source) in transaction.changed_files {
                    let Some(current) = state.files.get_mut(&path) else {
                        return Err(ArtifactEditorStoreError::InvalidFileSet);
                    };
                    *current = source;
                }
                state.revision = transaction.revision;
                state.active_path = transaction.active_path;
                state.view = transaction.view;
                state.selections = transaction.selections;
                validate_stored_context(
                    &state,
                    task_id,
                    artifact_id,
                    base_source_revision,
                    &baseline_digest,
                    entrypoint,
                    baseline_files,
                )?;
            }
        }
        if migration_required {
            write_snapshot(&task_root, artifact_id, &state)?;
            replace_journal_with_empty(&task_root, artifact_id)?;
        }
        checkpoint_from_stored(state)
    }

    pub(crate) fn save(
        &self,
        task_id: TaskId,
        artifact_id: ArtifactId,
        entrypoint: &str,
        baseline_files: &BTreeMap<String, String>,
        request: ArtifactEditorCheckpointRequest,
    ) -> Result<ArtifactEditorCheckpoint, ArtifactEditorStoreError> {
        let current = self.load(
            task_id,
            artifact_id,
            request.base_source_revision,
            entrypoint,
            baseline_files,
        )?;
        if request.expected_revision != current.revision {
            return Err(ArtifactEditorStoreError::StaleRevision {
                expected: current.revision,
                actual: request.expected_revision,
            });
        }
        validate_fixed_file_set(baseline_files, &request.files)?;
        validate_editor_state(
            entrypoint,
            &request.files,
            &request.active_path,
            &request.selections,
        )?;
        if current.files == request.files
            && current.active_path == request.active_path
            && current.view == request.view
            && current.selections == request.selections
        {
            return Ok(current);
        }
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(ArtifactEditorStoreError::RevisionOverflow)?;
        let changed_files = request
            .files
            .iter()
            .filter(|(path, source)| current.files.get(*path) != Some(*source))
            .map(|(path, source)| (path.clone(), source.clone()))
            .collect();
        let transaction = EditorTransaction {
            schema_version: EDITOR_STATE_SCHEMA_VERSION,
            task_id,
            artifact_id,
            base_source_revision: request.base_source_revision,
            baseline_digest: Some(editor_baseline_digest(
                request.base_source_revision,
                entrypoint,
                baseline_files,
            )?),
            revision,
            changed_files,
            active_path: request.active_path.clone(),
            view: request.view,
            selections: request.selections.clone(),
        };
        let task_root = self.ensure_task_root(task_id)?;
        append_transaction(&self.journal_path(task_id, artifact_id), &transaction)?;
        let stored = StoredEditorCheckpoint {
            schema_version: EDITOR_STATE_SCHEMA_VERSION,
            task_id,
            artifact_id,
            base_source_revision: request.base_source_revision,
            baseline_digest: transaction.baseline_digest.clone(),
            revision,
            entrypoint: entrypoint.to_owned(),
            files: request.files,
            active_path: request.active_path,
            view: request.view,
            selections: request.selections,
        };
        let journal_bytes = fs::metadata(self.journal_path(task_id, artifact_id))?.len();
        if revision % COMPACT_EVERY_REVISIONS == 0 || journal_bytes > MAX_EDITOR_JOURNAL_BYTES / 2 {
            write_snapshot(&task_root, artifact_id, &stored)?;
            replace_journal_with_empty(&task_root, artifact_id)?;
        }
        checkpoint_from_stored(stored)
    }

    fn task_root(&self, task_id: TaskId) -> PathBuf {
        self.root.join(task_id.to_string())
    }

    fn ensure_task_root(&self, task_id: TaskId) -> Result<PathBuf, ArtifactEditorStoreError> {
        let task_root = self.task_root(task_id);
        create_private_directory(&task_root)?;
        File::open(&self.root)?.sync_all()?;
        Ok(task_root)
    }

    fn snapshot_path(&self, task_id: TaskId, artifact_id: ArtifactId) -> PathBuf {
        self.task_root(task_id)
            .join(format!("{artifact_id}.snapshot.json"))
    }

    fn journal_path(&self, task_id: TaskId, artifact_id: ArtifactId) -> PathBuf {
        self.task_root(task_id)
            .join(format!("{artifact_id}.journal.jsonl"))
    }
}

fn migrate_stored_checkpoint(
    state: &mut StoredEditorCheckpoint,
    expected_baseline_digest: &str,
) -> Result<bool, ArtifactEditorStoreError> {
    match state.schema_version {
        LEGACY_EDITOR_STATE_SCHEMA_VERSION if state.baseline_digest.is_none() => {
            state.schema_version = EDITOR_STATE_SCHEMA_VERSION;
            state.baseline_digest = Some(expected_baseline_digest.to_owned());
            Ok(true)
        }
        EDITOR_STATE_SCHEMA_VERSION
            if state.baseline_digest.as_deref() == Some(expected_baseline_digest) =>
        {
            Ok(false)
        }
        LEGACY_EDITOR_STATE_SCHEMA_VERSION | EDITOR_STATE_SCHEMA_VERSION => {
            Err(ArtifactEditorStoreError::ContextMismatch)
        }
        version => Err(ArtifactEditorStoreError::UnsupportedSchema(version)),
    }
}

fn migrate_transaction(
    transaction: &mut EditorTransaction,
    expected_baseline_digest: &str,
) -> Result<bool, ArtifactEditorStoreError> {
    match transaction.schema_version {
        LEGACY_EDITOR_STATE_SCHEMA_VERSION if transaction.baseline_digest.is_none() => {
            transaction.schema_version = EDITOR_STATE_SCHEMA_VERSION;
            transaction.baseline_digest = Some(expected_baseline_digest.to_owned());
            Ok(true)
        }
        EDITOR_STATE_SCHEMA_VERSION
            if transaction.baseline_digest.as_deref() == Some(expected_baseline_digest) =>
        {
            Ok(false)
        }
        LEGACY_EDITOR_STATE_SCHEMA_VERSION | EDITOR_STATE_SCHEMA_VERSION => {
            Err(ArtifactEditorStoreError::ContextMismatch)
        }
        version => Err(ArtifactEditorStoreError::UnsupportedSchema(version)),
    }
}

fn validate_stored_context(
    state: &StoredEditorCheckpoint,
    task_id: TaskId,
    artifact_id: ArtifactId,
    base_source_revision: u64,
    baseline_digest: &str,
    entrypoint: &str,
    baseline_files: &BTreeMap<String, String>,
) -> Result<(), ArtifactEditorStoreError> {
    if state.schema_version != EDITOR_STATE_SCHEMA_VERSION
        || state.task_id != task_id
        || state.artifact_id != artifact_id
        || state.base_source_revision != base_source_revision
        || state.baseline_digest.as_deref() != Some(baseline_digest)
        || state.entrypoint != entrypoint
    {
        return Err(ArtifactEditorStoreError::ContextMismatch);
    }
    validate_fixed_file_set(baseline_files, &state.files)?;
    validate_editor_state(
        &state.entrypoint,
        &state.files,
        &state.active_path,
        &state.selections,
    )
}

fn validate_transaction_context(
    transaction: &EditorTransaction,
    task_id: TaskId,
    artifact_id: ArtifactId,
    base_source_revision: u64,
    baseline_digest: &str,
) -> Result<(), ArtifactEditorStoreError> {
    if transaction.schema_version != EDITOR_STATE_SCHEMA_VERSION
        || transaction.task_id != task_id
        || transaction.artifact_id != artifact_id
        || transaction.base_source_revision != base_source_revision
        || transaction.baseline_digest.as_deref() != Some(baseline_digest)
        || transaction.revision == 0
        || transaction.changed_files.len() > MAX_EDITOR_FILES
    {
        return Err(ArtifactEditorStoreError::ContextMismatch);
    }
    Ok(())
}

fn validate_fixed_file_set(
    baseline: &BTreeMap<String, String>,
    files: &BTreeMap<String, String>,
) -> Result<(), ArtifactEditorStoreError> {
    if baseline.keys().ne(files.keys()) {
        return Err(ArtifactEditorStoreError::InvalidFileSet);
    }
    Ok(())
}

fn validate_editor_state(
    entrypoint: &str,
    files: &BTreeMap<String, String>,
    active_path: &str,
    selections: &BTreeMap<String, ArtifactEditorSelection>,
) -> Result<(), ArtifactEditorStoreError> {
    if files.is_empty()
        || files.len() > MAX_EDITOR_FILES
        || !files.contains_key(entrypoint)
        || !files.contains_key(active_path)
        || selections.len() > MAX_EDITOR_SELECTIONS
        || selections.keys().any(|path| !files.contains_key(path))
        || files.keys().any(|path| !valid_virtual_path(path))
    {
        return Err(ArtifactEditorStoreError::InvalidEditorState);
    }
    let source_bytes = files.iter().try_fold(0_usize, |total, (path, source)| {
        total
            .checked_add(path.len())
            .and_then(|bytes| bytes.checked_add(source.len()))
    });
    if source_bytes.is_none_or(|bytes| bytes > MAX_EDITOR_SOURCE_BYTES)
        || selections.values().any(|selection| {
            selection.anchor as usize > MAX_EDITOR_SOURCE_BYTES
                || selection.head as usize > MAX_EDITOR_SOURCE_BYTES
        })
    {
        return Err(ArtifactEditorStoreError::TooLarge);
    }
    Ok(())
}

fn valid_virtual_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= MAX_GENUI_VIRTUAL_PATH_BYTES
        && !path.contains('\0')
        && !path.contains('\\')
        && !path.split('/').any(|segment| segment == "..")
}

fn checkpoint_from_stored(
    stored: StoredEditorCheckpoint,
) -> Result<ArtifactEditorCheckpoint, ArtifactEditorStoreError> {
    let state_digest = editor_state_digest(&stored)?;
    Ok(ArtifactEditorCheckpoint {
        schema_version: stored.schema_version,
        artifact_id: stored.artifact_id,
        base_source_revision: stored.base_source_revision,
        revision: stored.revision,
        state_digest,
        entrypoint: stored.entrypoint,
        files: stored.files,
        active_path: stored.active_path,
        view: stored.view,
        selections: stored.selections,
    })
}

fn editor_state_digest(state: &StoredEditorCheckpoint) -> Result<String, ArtifactEditorStoreError> {
    let encoded = serde_json::to_vec(&(
        state.schema_version,
        state.artifact_id,
        state.base_source_revision,
        &state.baseline_digest,
        state.revision,
        &state.entrypoint,
        &state.files,
        &state.active_path,
        state.view,
        &state.selections,
    ))?;
    Ok(hex_digest(Sha256::digest(encoded)))
}

fn editor_baseline_digest(
    base_source_revision: u64,
    entrypoint: &str,
    baseline_files: &BTreeMap<String, String>,
) -> Result<String, ArtifactEditorStoreError> {
    let encoded = serde_json::to_vec(&(base_source_revision, entrypoint, baseline_files))?;
    Ok(hex_digest(Sha256::digest(encoded)))
}

fn append_transaction(
    path: &Path,
    transaction: &EditorTransaction,
) -> Result<(), ArtifactEditorStoreError> {
    let mut encoded = serde_json::to_vec(transaction)?;
    if encoded.len() as u64 > MAX_EDITOR_STATE_BYTES {
        return Err(ArtifactEditorStoreError::TooLarge);
    }
    encoded.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    File::open(path.parent().ok_or(ArtifactEditorStoreError::InvalidPath)?)?.sync_all()?;
    Ok(())
}

fn write_snapshot(
    task_root: &Path,
    artifact_id: ArtifactId,
    state: &StoredEditorCheckpoint,
) -> Result<(), ArtifactEditorStoreError> {
    let encoded = serde_json::to_vec(state)?;
    if encoded.len() as u64 > MAX_EDITOR_STATE_BYTES {
        return Err(ArtifactEditorStoreError::TooLarge);
    }
    let target = task_root.join(format!("{artifact_id}.snapshot.json"));
    atomic_replace(task_root, &target, &encoded)
}

fn replace_journal_with_empty(
    task_root: &Path,
    artifact_id: ArtifactId,
) -> Result<(), ArtifactEditorStoreError> {
    let target = task_root.join(format!("{artifact_id}.journal.jsonl"));
    atomic_replace(task_root, &target, &[])
}

fn atomic_replace(
    parent: &Path,
    target: &Path,
    bytes: &[u8],
) -> Result<(), ArtifactEditorStoreError> {
    let temporary = parent.join(format!(".editor-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, target)?;
        File::open(parent)?.sync_all()?;
        Ok::<(), ArtifactEditorStoreError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, ArtifactEditorStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(ArtifactEditorStoreError::InvalidPath);
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
        return Err(ArtifactEditorStoreError::TooLarge);
    }
    Ok(bytes)
}

fn create_private_directory(path: &Path) -> Result<(), ArtifactEditorStoreError> {
    if path.exists() {
        validate_private_directory(path)?;
    } else {
        fs::create_dir(path)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), ArtifactEditorStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactEditorStoreError::InvalidPath);
    }
    Ok(())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Error)]
pub(crate) enum ArtifactEditorStoreError {
    #[error("artifact editor state path is invalid")]
    InvalidPath,
    #[error("artifact editor state context does not match the current artifact")]
    ContextMismatch,
    #[error("artifact editor state schema {0} is not supported")]
    UnsupportedSchema(u32),
    #[error("artifact editor state changed virtual file paths")]
    InvalidFileSet,
    #[error("artifact editor state is invalid")]
    InvalidEditorState,
    #[error("artifact editor state exceeds its bound")]
    TooLarge,
    #[error("artifact editor journal has a torn tail")]
    TornJournal,
    #[error("artifact editor journal has a revision gap")]
    RevisionGap,
    #[error("artifact editor revision is stale: expected {expected}, got {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("artifact editor revision overflowed")]
    RevisionOverflow,
    #[error("artifact editor state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact editor state JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("/App.tsx".into(), "export default () => null;\n".into()),
            ("/theme.ts".into(), "export const color = 'green';\n".into()),
        ])
    }

    #[test]
    fn checkpoint_reopens_from_versioned_transactions() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactEditorStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let mut files = baseline();
        files.insert("/theme.ts".into(), "export const color = 'amber';\n".into());
        let saved = store
            .save(
                task_id,
                artifact_id,
                "/App.tsx",
                &baseline(),
                ArtifactEditorCheckpointRequest {
                    expected_revision: 0,
                    base_source_revision: 7,
                    files: files.clone(),
                    active_path: "/theme.ts".into(),
                    view: ArtifactEditorView::Diff,
                    selections: BTreeMap::from([(
                        "/theme.ts".into(),
                        ArtifactEditorSelection {
                            anchor: 7,
                            head: 12,
                        },
                    )]),
                },
            )
            .unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.state_digest.len(), 64);

        let reopened = ArtifactEditorStore::open(temporary.path())
            .unwrap()
            .load(task_id, artifact_id, 7, "/App.tsx", &baseline())
            .unwrap();
        assert_eq!(reopened.files, files);
        assert_eq!(reopened.active_path, "/theme.ts");
        assert_eq!(reopened.view, ArtifactEditorView::Diff);
        assert_eq!(reopened.selections["/theme.ts"].head, 12);
    }

    #[test]
    fn stale_revision_and_changed_path_set_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactEditorStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let request = |revision, files| ArtifactEditorCheckpointRequest {
            expected_revision: revision,
            base_source_revision: 3,
            files,
            active_path: "/App.tsx".into(),
            view: ArtifactEditorView::Code,
            selections: BTreeMap::new(),
        };
        let mut changed = baseline();
        changed.insert("/App.tsx".into(), "export default 42;\n".into());
        store
            .save(
                task_id,
                artifact_id,
                "/App.tsx",
                &baseline(),
                request(0, changed.clone()),
            )
            .unwrap();
        assert!(matches!(
            store.save(
                task_id,
                artifact_id,
                "/App.tsx",
                &baseline(),
                request(0, changed)
            ),
            Err(ArtifactEditorStoreError::StaleRevision { .. })
        ));
        let mut missing = baseline();
        missing.remove("/theme.ts");
        assert!(matches!(
            store.save(
                task_id,
                artifact_id,
                "/App.tsx",
                &baseline(),
                request(1, missing)
            ),
            Err(ArtifactEditorStoreError::InvalidFileSet)
        ));
    }

    #[test]
    fn compaction_preserves_the_latest_selection_and_torn_journals_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactEditorStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        for revision in 0..COMPACT_EVERY_REVISIONS {
            store
                .save(
                    task_id,
                    artifact_id,
                    "/App.tsx",
                    &baseline(),
                    ArtifactEditorCheckpointRequest {
                        expected_revision: revision,
                        base_source_revision: 9,
                        files: baseline(),
                        active_path: if revision % 2 == 0 {
                            "/theme.ts".into()
                        } else {
                            "/App.tsx".into()
                        },
                        view: ArtifactEditorView::Code,
                        selections: BTreeMap::from([(
                            "/App.tsx".into(),
                            ArtifactEditorSelection {
                                anchor: revision as u32,
                                head: revision as u32 + 1,
                            },
                        )]),
                    },
                )
                .unwrap();
        }
        let reopened = store
            .load(task_id, artifact_id, 9, "/App.tsx", &baseline())
            .unwrap();
        assert_eq!(reopened.revision, COMPACT_EVERY_REVISIONS);
        assert_eq!(reopened.selections["/App.tsx"].head, 64);

        fs::write(store.journal_path(task_id, artifact_id), b"{\"torn\":true}").unwrap();
        assert!(matches!(
            store.load(task_id, artifact_id, 9, "/App.tsx", &baseline()),
            Err(ArtifactEditorStoreError::TornJournal)
        ));
    }

    #[test]
    fn legacy_snapshot_and_journal_migrate_to_a_bound_v2_checkpoint() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactEditorStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let task_root = store.ensure_task_root(task_id).unwrap();
        let mut snapshot_files = baseline();
        snapshot_files.insert("/theme.ts".into(), "export const color = 'amber';\n".into());
        write_snapshot(
            &task_root,
            artifact_id,
            &StoredEditorCheckpoint {
                schema_version: LEGACY_EDITOR_STATE_SCHEMA_VERSION,
                task_id,
                artifact_id,
                base_source_revision: 7,
                baseline_digest: None,
                revision: 1,
                entrypoint: "/App.tsx".into(),
                files: snapshot_files,
                active_path: "/theme.ts".into(),
                view: ArtifactEditorView::Diff,
                selections: BTreeMap::new(),
            },
        )
        .unwrap();
        append_transaction(
            &store.journal_path(task_id, artifact_id),
            &EditorTransaction {
                schema_version: LEGACY_EDITOR_STATE_SCHEMA_VERSION,
                task_id,
                artifact_id,
                base_source_revision: 7,
                baseline_digest: None,
                revision: 2,
                changed_files: BTreeMap::from([(
                    "/App.tsx".into(),
                    "export default () => 'migrated';\n".into(),
                )]),
                active_path: "/App.tsx".into(),
                view: ArtifactEditorView::Code,
                selections: BTreeMap::from([(
                    "/App.tsx".into(),
                    ArtifactEditorSelection {
                        anchor: 4,
                        head: 12,
                    },
                )]),
            },
        )
        .unwrap();

        let migrated = store
            .load(task_id, artifact_id, 7, "/App.tsx", &baseline())
            .unwrap();
        assert_eq!(migrated.schema_version, EDITOR_STATE_SCHEMA_VERSION);
        assert_eq!(migrated.revision, 2);
        assert_eq!(
            migrated.files["/App.tsx"],
            "export default () => 'migrated';\n"
        );
        assert_eq!(migrated.selections["/App.tsx"].head, 12);

        let encoded = read_bounded_file(
            &store.snapshot_path(task_id, artifact_id),
            MAX_EDITOR_STATE_BYTES,
        )
        .unwrap();
        let snapshot: StoredEditorCheckpoint = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(snapshot.schema_version, EDITOR_STATE_SCHEMA_VERSION);
        assert_eq!(
            snapshot.baseline_digest.as_deref(),
            Some(
                editor_baseline_digest(7, "/App.tsx", &baseline())
                    .unwrap()
                    .as_str()
            )
        );
        assert_eq!(
            fs::metadata(store.journal_path(task_id, artifact_id))
                .unwrap()
                .len(),
            0
        );

        let reopened = ArtifactEditorStore::open(temporary.path())
            .unwrap()
            .load(task_id, artifact_id, 7, "/App.tsx", &baseline())
            .unwrap();
        assert_eq!(reopened, migrated);
    }

    #[test]
    fn bound_v2_rejects_a_different_baseline_and_future_schema() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ArtifactEditorStore::open(temporary.path()).unwrap();
        let task_id = TaskId::new();
        let artifact_id = ArtifactId::new();
        let task_root = store.ensure_task_root(task_id).unwrap();
        let mut checkpoint = StoredEditorCheckpoint {
            schema_version: EDITOR_STATE_SCHEMA_VERSION,
            task_id,
            artifact_id,
            base_source_revision: 11,
            baseline_digest: Some("0".repeat(64)),
            revision: 1,
            entrypoint: "/App.tsx".into(),
            files: baseline(),
            active_path: "/App.tsx".into(),
            view: ArtifactEditorView::Code,
            selections: BTreeMap::new(),
        };
        write_snapshot(&task_root, artifact_id, &checkpoint).unwrap();
        assert!(matches!(
            store.load(task_id, artifact_id, 11, "/App.tsx", &baseline()),
            Err(ArtifactEditorStoreError::ContextMismatch)
        ));

        checkpoint.schema_version = EDITOR_STATE_SCHEMA_VERSION + 1;
        checkpoint.baseline_digest =
            Some(editor_baseline_digest(11, "/App.tsx", &baseline()).unwrap());
        write_snapshot(&task_root, artifact_id, &checkpoint).unwrap();
        assert!(matches!(
            store.load(task_id, artifact_id, 11, "/App.tsx", &baseline()),
            Err(ArtifactEditorStoreError::UnsupportedSchema(version))
                if version == EDITOR_STATE_SCHEMA_VERSION + 1
        ));
    }
}
