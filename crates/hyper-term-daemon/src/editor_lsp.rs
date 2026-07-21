use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use hyper_term_drivers::{DenoLspClient, DenoLspConfig, path_to_file_uri};
use hyper_term_protocol::{
    ArtifactId, MAX_GENUI_SOURCE_BYTES, MAX_GENUI_SOURCE_FILES, MAX_GENUI_VIRTUAL_PATH_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::artifact_store::StoredGenUiArtifact;

const LSP_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const LSP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const LSP_DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(5);
const LSP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_EDITOR_DRAFT_FILES: usize = MAX_GENUI_SOURCE_FILES;
const MAX_EDITOR_DRAFT_BYTES: usize = MAX_GENUI_SOURCE_BYTES;
const MAX_EDITOR_DOCUMENT_PATH_BYTES: usize = MAX_GENUI_VIRTUAL_PATH_BYTES;
const MAX_EDITOR_COMPLETIONS: usize = 128;
const MAX_EDITOR_DIAGNOSTICS: usize = 256;
const MAX_EDITOR_LABEL_BYTES: usize = 512;
const MAX_EDITOR_DETAIL_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EditorLspRequestKind {
    Diagnostics,
    Completion,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditorLspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditorLspRequest {
    pub source_revision: u64,
    pub document_path: String,
    pub draft_files: BTreeMap<String, String>,
    pub kind: EditorLspRequestKind,
    #[serde(default)]
    pub position: Option<EditorLspPosition>,
}

impl EditorLspRequest {
    pub(crate) fn validate(&self) -> Result<(), EditorLspError> {
        if self.source_revision == 0 {
            return Err(EditorLspError::InvalidRequest(
                "source_revision must be positive".into(),
            ));
        }
        if self.draft_files.is_empty() || self.draft_files.len() > MAX_EDITOR_DRAFT_FILES {
            return Err(EditorLspError::InvalidRequest(format!(
                "draft must contain between 1 and {MAX_EDITOR_DRAFT_FILES} files"
            )));
        }
        let mut draft_bytes = 0usize;
        for (path, source) in &self.draft_files {
            validate_virtual_path(path)?;
            draft_bytes = draft_bytes
                .checked_add(path.len())
                .and_then(|bytes| bytes.checked_add(source.len()))
                .ok_or_else(|| EditorLspError::InvalidRequest("draft size overflowed".into()))?;
        }
        if draft_bytes > MAX_EDITOR_DRAFT_BYTES {
            return Err(EditorLspError::InvalidRequest(format!(
                "draft exceeds the {MAX_EDITOR_DRAFT_BYTES}-byte editor bound"
            )));
        }
        validate_virtual_path(&self.document_path)?;
        let source = self.draft_files.get(&self.document_path).ok_or_else(|| {
            EditorLspError::InvalidRequest("document_path must identify a file in the draft".into())
        })?;
        match self.kind {
            EditorLspRequestKind::Diagnostics if self.position.is_some() => {
                return Err(EditorLspError::InvalidRequest(
                    "diagnostics does not accept a position".into(),
                ));
            }
            EditorLspRequestKind::Completion => {
                let position = self.position.ok_or_else(|| {
                    EditorLspError::InvalidRequest("completion requires a position".into())
                })?;
                let line = source
                    .split('\n')
                    .nth(position.line as usize)
                    .ok_or_else(|| {
                        EditorLspError::InvalidRequest(
                            "completion position is outside the document".into(),
                        )
                    })?;
                if position.character as usize > line.encode_utf16().count() {
                    return Err(EditorLspError::InvalidRequest(
                        "completion position is outside the document".into(),
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct EditorLspDiagnostic {
    pub severity: String,
    pub message: String,
    pub start: EditorLspPosition,
    pub end: EditorLspPosition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct EditorLspCompletion {
    pub label: String,
    pub insert_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EditorLspResponse {
    pub artifact_id: ArtifactId,
    pub source_revision: u64,
    pub document_path: String,
    pub document_version: i32,
    pub kind: String,
    pub diagnostics: Vec<EditorLspDiagnostic>,
    pub completions: Vec<EditorLspCompletion>,
}

pub(crate) struct EditorLspService {
    config: EditorLspConfig,
    sessions: Mutex<HashMap<EditorLspKey, Arc<Mutex<ArtifactLspSession>>>>,
}

struct EditorLspConfig {
    executable: PathBuf,
    executable_sha256: String,
    runtime_version: String,
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EditorLspKey {
    session_id: u16,
    artifact_id: ArtifactId,
    source_revision: u64,
}

struct ArtifactLspSession {
    client: DenoLspClient,
    root: PathBuf,
    documents: HashMap<String, EditorDocument>,
}

struct EditorDocument {
    uri: String,
    language_id: &'static str,
    version: i32,
    source: Option<String>,
    diagnostics_version: Option<i32>,
    diagnostics: Vec<EditorLspDiagnostic>,
}

impl EditorLspService {
    pub(crate) fn new(
        executable: PathBuf,
        executable_sha256: String,
        runtime_version: String,
        state_directory: &Path,
    ) -> Result<Self, EditorLspError> {
        if runtime_version.is_empty() {
            return Err(EditorLspError::InvalidRuntime);
        }
        let executable = executable
            .canonicalize()
            .map_err(|_| EditorLspError::InvalidRuntime)?;
        let root = state_directory
            .join("editor-lsp")
            .join(format!("run-{}", Uuid::new_v4()));
        create_private_directory(&root)?;
        Ok(Self {
            config: EditorLspConfig {
                executable,
                executable_sha256,
                runtime_version,
                root: root.canonicalize()?,
            },
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn query(
        &self,
        session_id: u16,
        artifact: &StoredGenUiArtifact,
        request: EditorLspRequest,
    ) -> Result<EditorLspResponse, EditorLspError> {
        request.validate()?;
        if request.source_revision != artifact.metadata.source_revision {
            return Err(EditorLspError::StaleRevision);
        }
        validate_draft_inventory(&request.draft_files, &artifact.source_files)?;
        let key = EditorLspKey {
            session_id,
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
        };
        let session = self.session(key, artifact)?;
        lock(&session)?.query(artifact.metadata.artifact_id, request)
    }

    pub(crate) fn close_session(&self, session_id: u16) {
        let removed = if let Ok(mut sessions) = self.sessions.lock() {
            let keys = sessions
                .keys()
                .filter(|key| key.session_id == session_id)
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| sessions.remove(&key))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        close_lsp_sessions(removed);
    }

    pub(crate) fn close_all(&self) {
        let removed = if let Ok(mut sessions) = self.sessions.lock() {
            sessions.drain().map(|(_, session)| session).collect()
        } else {
            Vec::new()
        };
        close_lsp_sessions(removed);
        let _ = fs::remove_dir_all(&self.config.root);
    }

    fn session(
        &self,
        key: EditorLspKey,
        artifact: &StoredGenUiArtifact,
    ) -> Result<Arc<Mutex<ArtifactLspSession>>, EditorLspError> {
        if let Some(session) = lock(&self.sessions)?.get(&key).cloned() {
            return Ok(session);
        }
        let launched = Arc::new(Mutex::new(ArtifactLspSession::launch(
            &self.config,
            &key,
            &artifact.source_files,
        )?));
        let mut sessions = lock(&self.sessions)?;
        if let Some(existing) = sessions.get(&key).cloned() {
            drop(sessions);
            close_lsp_sessions(vec![launched]);
            Ok(existing)
        } else {
            sessions.insert(key, launched.clone());
            Ok(launched)
        }
    }
}

impl Drop for EditorLspService {
    fn drop(&mut self) {
        if let Ok(sessions) = self.sessions.get_mut() {
            close_lsp_sessions(sessions.drain().map(|(_, session)| session).collect());
        }
        let _ = fs::remove_dir_all(&self.config.root);
    }
}

impl ArtifactLspSession {
    fn launch(
        config: &EditorLspConfig,
        key: &EditorLspKey,
        source_files: &BTreeMap<String, String>,
    ) -> Result<Self, EditorLspError> {
        let root = config.root.join(format!(
            "session-{}/artifact-{}-r{}",
            key.session_id, key.artifact_id, key.source_revision
        ));
        let workspace = root.join("workspace");
        let cache = root.join("cache");
        let scratch = root.join("scratch");
        for directory in [&workspace, &cache, &scratch] {
            create_private_directory(directory)?;
        }
        let mut documents = HashMap::new();
        for (virtual_path, source) in source_files {
            validate_virtual_path(virtual_path)?;
            let relative = virtual_path
                .strip_prefix('/')
                .ok_or_else(|| EditorLspError::InvalidRequest("virtual path is invalid".into()))?;
            let destination = workspace.join(relative);
            if let Some(parent) = destination.parent() {
                create_private_descendant(&workspace, parent)?;
            }
            write_private_file(&destination, source.as_bytes())?;
            let canonical = destination.canonicalize()?;
            if !canonical.starts_with(workspace.canonicalize()?) {
                return Err(EditorLspError::DocumentUnavailable);
            }
            documents.insert(
                virtual_path.clone(),
                EditorDocument {
                    uri: path_to_file_uri(&canonical)
                        .map_err(|error| EditorLspError::Driver(error.to_string()))?,
                    language_id: language_id(&canonical),
                    version: 0,
                    source: None,
                    diagnostics_version: None,
                    diagnostics: Vec::new(),
                },
            );
        }
        let client = DenoLspClient::launch(DenoLspConfig {
            executable: config.executable.clone(),
            executable_sha256: config.executable_sha256.clone(),
            runtime_version: config.runtime_version.clone(),
            workspace_snapshot: workspace.canonicalize()?,
            cache_directory: cache.canonicalize()?,
            scratch_directory: scratch.canonicalize()?,
        })
        .map_err(|error| EditorLspError::Driver(error.to_string()))?;
        client
            .initialize(LSP_INITIALIZE_TIMEOUT)
            .map_err(|error| EditorLspError::Driver(error.to_string()))?;
        Ok(Self {
            client,
            root: root.canonicalize()?,
            documents,
        })
    }

    fn query(
        &mut self,
        artifact_id: ArtifactId,
        request: EditorLspRequest,
    ) -> Result<EditorLspResponse, EditorLspError> {
        self.sync_draft(&request.document_path, &request.draft_files)?;
        let document = self
            .documents
            .get_mut(&request.document_path)
            .ok_or(EditorLspError::DocumentUnavailable)?;
        let (diagnostics, completions) = match request.kind {
            EditorLspRequestKind::Diagnostics => {
                if document.diagnostics_version != Some(document.version) {
                    let notification = wait_for_document_diagnostics(
                        &self.client,
                        &document.uri,
                        document.version,
                    )?;
                    document.diagnostics =
                        normalize_diagnostics(notification.pointer("/params/diagnostics"))?;
                    document.diagnostics_version = Some(document.version);
                }
                (document.diagnostics.clone(), Vec::new())
            }
            EditorLspRequestKind::Completion => {
                let position = request.position.expect("validated completion position");
                let response = self
                    .client
                    .request(
                        "textDocument/completion",
                        json!({
                            "textDocument": {"uri": document.uri},
                            "position": position,
                            "context": {"triggerKind": 1}
                        }),
                        LSP_REQUEST_TIMEOUT,
                    )
                    .map_err(|error| EditorLspError::Driver(error.to_string()))?;
                (Vec::new(), normalize_completions(response.get("result"))?)
            }
        };
        Ok(EditorLspResponse {
            artifact_id,
            source_revision: request.source_revision,
            document_path: request.document_path,
            document_version: document.version,
            kind: match request.kind {
                EditorLspRequestKind::Diagnostics => "diagnostics",
                EditorLspRequestKind::Completion => "completion",
            }
            .into(),
            diagnostics,
            completions,
        })
    }

    fn sync_draft(
        &mut self,
        active_path: &str,
        draft_files: &BTreeMap<String, String>,
    ) -> Result<(), EditorLspError> {
        let mut changed = false;
        for (path, source) in draft_files {
            if path == active_path {
                continue;
            }
            let document = self
                .documents
                .get_mut(path)
                .ok_or(EditorLspError::DocumentUnavailable)?;
            changed |= sync_document(&self.client, document, source, false)?;
        }
        let active_source = draft_files
            .get(active_path)
            .ok_or(EditorLspError::DocumentUnavailable)?;
        let active = self
            .documents
            .get_mut(active_path)
            .ok_or(EditorLspError::DocumentUnavailable)?;
        changed |= sync_document(&self.client, active, active_source, changed)?;
        if changed {
            for document in self.documents.values_mut() {
                document.diagnostics_version = None;
            }
        }
        Ok(())
    }
}

fn wait_for_document_diagnostics(
    client: &DenoLspClient,
    uri: &str,
    version: i32,
) -> Result<Value, EditorLspError> {
    let deadline = Instant::now() + LSP_DIAGNOSTIC_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(EditorLspError::Driver(
                "Deno LSP diagnostics timed out".into(),
            ));
        }
        let notification = client
            .wait_for_notification("textDocument/publishDiagnostics", remaining)
            .map_err(|error| EditorLspError::Driver(error.to_string()))?;
        if notification.pointer("/params/uri").and_then(Value::as_str) != Some(uri) {
            continue;
        }
        let notification_version = notification.pointer("/params/version");
        if notification_version.is_some()
            && notification_version.and_then(Value::as_i64) != Some(i64::from(version))
        {
            continue;
        }
        return Ok(notification);
    }
}

fn sync_document(
    client: &DenoLspClient,
    document: &mut EditorDocument,
    source: &str,
    force: bool,
) -> Result<bool, EditorLspError> {
    if document.source.as_deref() == Some(source) && !force {
        return Ok(false);
    }
    document.version = document
        .version
        .checked_add(1)
        .ok_or(EditorLspError::VersionExhausted)?;
    if document.source.is_none() {
        client
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": document.uri,
                        "languageId": document.language_id,
                        "version": document.version,
                        "text": source
                    }
                }),
            )
            .map_err(|error| EditorLspError::Driver(error.to_string()))?;
    } else {
        client
            .notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {
                        "uri": document.uri,
                        "version": document.version
                    },
                    "contentChanges": [{"text": source}]
                }),
            )
            .map_err(|error| EditorLspError::Driver(error.to_string()))?;
    }
    document.source = Some(source.to_owned());
    document.diagnostics_version = None;
    Ok(true)
}

fn normalize_diagnostics(
    value: Option<&Value>,
) -> Result<Vec<EditorLspDiagnostic>, EditorLspError> {
    let items = value
        .and_then(Value::as_array)
        .ok_or(EditorLspError::UnexpectedResponse)?;
    if items.len() > MAX_EDITOR_DIAGNOSTICS {
        return Err(EditorLspError::UnexpectedResponse);
    }
    items
        .iter()
        .map(|item| {
            let message = bounded_string(item.get("message"), MAX_EDITOR_DETAIL_BYTES)?;
            let severity = match item.get("severity").and_then(Value::as_u64) {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "information",
                Some(4) => "hint",
                _ => "information",
            }
            .to_owned();
            Ok(EditorLspDiagnostic {
                severity,
                message,
                start: lsp_position(item.pointer("/range/start"))?,
                end: lsp_position(item.pointer("/range/end"))?,
            })
        })
        .collect()
}

fn normalize_completions(
    value: Option<&Value>,
) -> Result<Vec<EditorLspCompletion>, EditorLspError> {
    let items = match value {
        Some(Value::Array(items)) => items,
        Some(Value::Object(object)) => object
            .get("items")
            .and_then(Value::as_array)
            .ok_or(EditorLspError::UnexpectedResponse)?,
        Some(Value::Null) | None => return Ok(Vec::new()),
        _ => return Err(EditorLspError::UnexpectedResponse),
    };
    items
        .iter()
        .take(MAX_EDITOR_COMPLETIONS)
        .map(|item| {
            let label = bounded_string(item.get("label"), MAX_EDITOR_LABEL_BYTES)?;
            let insert_text = item
                .get("insertText")
                .or_else(|| item.pointer("/textEdit/newText"))
                .map(|value| bounded_string(Some(value), MAX_EDITOR_DETAIL_BYTES))
                .transpose()?
                .unwrap_or_else(|| label.clone());
            let detail = item
                .get("detail")
                .map(|value| bounded_string(Some(value), MAX_EDITOR_DETAIL_BYTES))
                .transpose()?;
            Ok(EditorLspCompletion {
                label,
                insert_text,
                detail,
                kind: item.get("kind").and_then(Value::as_u64),
            })
        })
        .collect()
}

fn lsp_position(value: Option<&Value>) -> Result<EditorLspPosition, EditorLspError> {
    let value = value
        .and_then(Value::as_object)
        .ok_or(EditorLspError::UnexpectedResponse)?;
    let line = value
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(EditorLspError::UnexpectedResponse)?;
    let character = value
        .get("character")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(EditorLspError::UnexpectedResponse)?;
    Ok(EditorLspPosition { line, character })
}

fn bounded_string(value: Option<&Value>, maximum: usize) -> Result<String, EditorLspError> {
    let value = value
        .and_then(Value::as_str)
        .ok_or(EditorLspError::UnexpectedResponse)?;
    if value.len() > maximum {
        return Err(EditorLspError::UnexpectedResponse);
    }
    Ok(value.to_owned())
}

fn validate_virtual_path(path: &str) -> Result<(), EditorLspError> {
    if path.is_empty()
        || path.len() > MAX_EDITOR_DOCUMENT_PATH_BYTES
        || !path.starts_with('/')
        || path.contains('\\')
        || Path::new(path)
            .components()
            .skip(1)
            .any(|component| !matches!(component, Component::Normal(_)))
        || ![".tsx", ".ts", ".jsx", ".js", ".json", ".jsonc"]
            .iter()
            .any(|extension| path.ends_with(extension))
    {
        return Err(EditorLspError::InvalidRequest(
            "document_path must be a bounded virtual TS/JS/JSON path".into(),
        ));
    }
    Ok(())
}

fn validate_draft_inventory(
    draft_files: &BTreeMap<String, String>,
    accepted_files: &BTreeMap<String, String>,
) -> Result<(), EditorLspError> {
    if draft_files.len() != accepted_files.len() || draft_files.keys().ne(accepted_files.keys()) {
        return Err(EditorLspError::InvalidRequest(
            "draft file inventory must match the accepted artifact".into(),
        ));
    }
    Ok(())
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("js") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("json") | Some("jsonc") => "json",
        _ => "plaintext",
    }
}

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn create_private_descendant(root: &Path, path: &Path) -> Result<(), std::io::Error> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| std::io::Error::other("private path escapes its root"))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component.as_os_str());
        create_private_directory(&current)?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

fn close_lsp_sessions(sessions: Vec<Arc<Mutex<ArtifactLspSession>>>) {
    for session in sessions {
        if let Ok(session) = session.lock() {
            let _ = session.client.shutdown(LSP_SHUTDOWN_TIMEOUT);
            let _ = fs::remove_dir_all(&session.root);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, EditorLspError> {
    mutex.lock().map_err(|_| EditorLspError::LockPoisoned)
}

#[derive(Debug, Error)]
pub(crate) enum EditorLspError {
    #[error("editor LSP runtime is invalid")]
    InvalidRuntime,
    #[error("editor LSP request is invalid: {0}")]
    InvalidRequest(String),
    #[error("editor LSP source revision is stale")]
    StaleRevision,
    #[error("editor LSP document is unavailable")]
    DocumentUnavailable,
    #[error("editor LSP returned an invalid or oversized response")]
    UnexpectedResponse,
    #[error("editor LSP document version is exhausted")]
    VersionExhausted,
    #[error("editor LSP driver failed: {0}")]
    Driver(String),
    #[error("editor LSP I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("editor LSP lock was poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper_term_protocol::{AcceptedGenUiArtifact, GenUiCompilerIdentity};

    #[test]
    fn requests_are_bounded_and_completion_requires_a_position() {
        let request = EditorLspRequest {
            source_revision: 2,
            document_path: "/App.tsx".into(),
            draft_files: BTreeMap::from([("/App.tsx".into(), "export default 1;".into())]),
            kind: EditorLspRequestKind::Completion,
            position: None,
        };
        assert!(matches!(
            request.validate(),
            Err(EditorLspError::InvalidRequest(_))
        ));
        assert!(validate_virtual_path("/../escape.ts").is_err());
        assert!(validate_virtual_path("/App.tsx").is_ok());

        let accepted = BTreeMap::from([
            ("/App.tsx".into(), "export default 1;".into()),
            ("/theme.ts".into(), "export const color = 'lime';".into()),
        ]);
        let missing = BTreeMap::from([("/App.tsx".into(), "export default 1;".into())]);
        let additional = BTreeMap::from([
            ("/App.tsx".into(), "export default 1;".into()),
            ("/theme.ts".into(), "export const color = 'lime';".into()),
            ("/escape.ts".into(), "export default 2;".into()),
        ]);
        assert!(validate_draft_inventory(&accepted, &accepted).is_ok());
        assert!(validate_draft_inventory(&missing, &accepted).is_err());
        assert!(validate_draft_inventory(&additional, &accepted).is_err());
    }

    #[test]
    fn completion_results_are_normalized_and_bounded() {
        let response = json!({
            "isIncomplete": false,
            "items": [{
                "label": "useState",
                "detail": "function useState<T>(initial: T): [T, Dispatch<T>]",
                "kind": 3,
                "textEdit": {"newText": "useState"}
            }]
        });
        let completions = normalize_completions(Some(&response)).unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].label, "useState");
        assert_eq!(completions[0].insert_text, "useState");
    }

    #[test]
    #[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
    fn real_deno_lsp_tracks_draft_diagnostics_and_completion() {
        let temporary = tempfile::tempdir().unwrap();
        let deno =
            PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
                .canonicalize()
                .unwrap();
        let service = EditorLspService::new(
            deno,
            std::env::var("HYPER_TERM_DENO_SHA256").expect("HYPER_TERM_DENO_SHA256"),
            "2.9.3".into(),
            temporary.path(),
        )
        .unwrap();
        let artifact_id = ArtifactId::new();
        let artifact = StoredGenUiArtifact {
            metadata: AcceptedGenUiArtifact {
                artifact_id,
                source_revision: 3,
                entrypoint: "/main.ts".into(),
                content_digest: "a".repeat(64),
                compiler: GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
            },
            source_files: BTreeMap::from([
                (
                    "/main.ts".into(),
                    "import { answer } from \"./value.ts\";\nconst result: string = answer;\n"
                        .into(),
                ),
                ("/value.ts".into(), "export const answer = \"ok\";\n".into()),
            ]),
            bundle: String::new(),
            css: String::new(),
            source_map: String::new(),
        };
        let mut cross_file_draft = artifact.source_files.clone();
        cross_file_draft.insert("/value.ts".into(), "export const answer = 42;\n".into());
        let diagnostics = service
            .query(
                8,
                &artifact,
                EditorLspRequest {
                    source_revision: 3,
                    document_path: "/main.ts".into(),
                    draft_files: cross_file_draft,
                    kind: EditorLspRequestKind::Diagnostics,
                    position: None,
                },
            )
            .unwrap();
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == "error"
                    && diagnostic
                        .message
                        .contains("not assignable to type 'string'")),
            "cross-file draft diagnostic was not observed: {:?}",
            diagnostics.diagnostics
        );

        let completion = service
            .query(
                8,
                &artifact,
                EditorLspRequest {
                    source_revision: 3,
                    document_path: "/main.ts".into(),
                    draft_files: BTreeMap::from([
                        (
                            "/main.ts".into(),
                            "const value = \"ok\";\nvalue.toUpperCase();\n".into(),
                        ),
                        ("/value.ts".into(), "export const answer = 42;\n".into()),
                    ]),
                    kind: EditorLspRequestKind::Completion,
                    position: Some(EditorLspPosition {
                        line: 1,
                        character: 6,
                    }),
                },
            )
            .unwrap();
        assert!(!completion.completions.is_empty());
        assert_eq!(completion.document_version, 2);
        let clean_diagnostics = service
            .query(
                8,
                &artifact,
                EditorLspRequest {
                    source_revision: 3,
                    document_path: "/main.ts".into(),
                    draft_files: BTreeMap::from([
                        (
                            "/main.ts".into(),
                            "const value = \"ok\";\nvalue.toUpperCase();\n".into(),
                        ),
                        ("/value.ts".into(), "export const answer = 42;\n".into()),
                    ]),
                    kind: EditorLspRequestKind::Diagnostics,
                    position: None,
                },
            )
            .unwrap();
        assert!(
            clean_diagnostics
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != "error")
        );
        assert_eq!(clean_diagnostics.document_version, 2);
        let private_root = service.config.root.clone();
        service.close_all();
        assert!(!private_root.exists());
    }
}
