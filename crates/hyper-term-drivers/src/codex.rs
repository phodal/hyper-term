use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hyper_term_protocol::PermissionDecision;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    AgentDriverEvent, AgentEffectAuthorization, AgentEffectKind, AgentEffectProposal, DriverError,
    DriverEvent, DriverFraming, DriverKind, DriverManifest, DriverProcess, DriverSpec, DriverState,
    ExternalRequestId, StructuredAgentProtocol, sha256_file,
};

const MAX_PENDING_APPROVALS: usize = 128;
const MAX_BUFFERED_MESSAGES: usize = 512;
const CODEX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const HYPER_TERM_MCP_TOOLS: &[&str] = &[
    "hyper_term.genui.compile",
    "hyper_term.lsp.query",
    "hyper_term.diff.review",
];

pub struct CodexMcpServerConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub arguments: Vec<OsString>,
}

pub struct CodexAppServerConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub implementation_version: String,
    pub workspace: PathBuf,
    pub codex_home: PathBuf,
    pub scratch_directory: PathBuf,
    /// The only credential material admitted into the isolated Codex home.
    /// On Unix this is staged as a private symlink after ownership and mode checks.
    pub auth_file: Option<PathBuf>,
    pub brokered_mcp_server: Option<CodexMcpServerConfig>,
}

pub struct CodexAppServerClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<VecDeque<DriverEvent>>,
    pending: Mutex<HashMap<ExternalRequestId, AgentEffectProposal>>,
    workspace: String,
    staged_auth_file: Option<PathBuf>,
}

impl CodexAppServerClient {
    pub fn launch(config: CodexAppServerConfig) -> Result<Self, CodexAdapterError> {
        if !config.workspace.is_absolute()
            || !config.codex_home.is_absolute()
            || !config.scratch_directory.is_absolute()
        {
            return Err(CodexAdapterError::InvalidConfig(
                "Codex adapter directories must be absolute".into(),
            ));
        }
        fs::create_dir_all(&config.codex_home)?;
        fs::create_dir_all(&config.scratch_directory)?;
        let workspace = config.workspace.canonicalize()?;
        let workspace_text = workspace
            .to_str()
            .ok_or_else(|| {
                CodexAdapterError::InvalidConfig("Codex workspace path is not UTF-8".into())
            })?
            .to_owned();
        let codex_home = config.codex_home.canonicalize()?;
        let scratch = config.scratch_directory.canonicalize()?;
        let staged_auth_file = stage_auth_file(config.auth_file.as_ref(), &codex_home)?;
        let environment = BTreeMap::from([
            ("CODEX_HOME".into(), codex_home.into_os_string()),
            ("HOME".into(), scratch.clone().into_os_string()),
            ("NO_COLOR".into(), OsString::from("1")),
            ("RUST_BACKTRACE".into(), OsString::from("0")),
            ("TERM".into(), OsString::from("dumb")),
            ("TMPDIR".into(), scratch.into_os_string()),
        ]);
        let arguments = codex_arguments(config.brokered_mcp_server.as_ref())?;
        let process = match DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id: Uuid::new_v4(),
                kind: DriverKind::CodexAppServer,
                implementation_version: config.implementation_version,
                protocol_version: "codex-app-server-v2".into(),
                capabilities: vec![
                    "threads".into(),
                    "turns".into(),
                    "streaming".into(),
                    "permission_proposals".into(),
                ],
                transport: "stdio-jsonl".into(),
                executable_sha256: config.executable_sha256,
                permission_profile: "codex-proposal-only-v1".into(),
            },
            executable: config.executable,
            arguments,
            working_directory: workspace,
            environment,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: CODEX_FRAME_BYTES,
        }) {
            Ok(process) => process,
            Err(error) => {
                remove_staged_auth_file(staged_auth_file.as_ref());
                return Err(error.into());
            }
        };
        Ok(Self {
            process,
            next_request_id: AtomicU64::new(1),
            request_gate: Mutex::new(()),
            inbox: Mutex::new(VecDeque::new()),
            pending: Mutex::new(HashMap::new()),
            workspace: workspace_text,
            staged_auth_file,
        })
    }

    pub fn initialize(&self, timeout: Duration) -> Result<Value, CodexAdapterError> {
        let response = self.request_raw(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "hyper-term",
                    "title": "Hyper Term",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {"experimentalApi": false}
            }),
            timeout,
        )?;
        self.process.mark_ready()?;
        Ok(response)
    }

    pub fn next_event(&self, timeout: Duration) -> Result<AgentDriverEvent, CodexAdapterError> {
        let event = if let Some(event) = lock(&self.inbox)?.pop_front() {
            event
        } else {
            self.process.recv_timeout(timeout)?
        };
        match event {
            DriverEvent::Message { sequence, payload } => self.normalize_message(sequence, payload),
            DriverEvent::ProtocolError { message } => Err(CodexAdapterError::Protocol(message)),
            DriverEvent::Exited { code, state } => Ok(AgentDriverEvent::Exited { code, state }),
        }
    }

    pub fn start_thread(&self, timeout: Duration) -> Result<String, CodexAdapterError> {
        let response = self.request_raw(
            "thread/start",
            json!({
                "cwd": self.workspace,
                "approvalPolicy": "on-request",
                "sandbox": "read-only",
                "ephemeral": false,
                "historyMode": "paginated"
            }),
            timeout,
        )?;
        response
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodexAdapterError::InvalidMessage("thread/start returned no thread id".into())
            })
            .and_then(|value| bounded(value.to_owned(), 4096))
    }

    pub fn start_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, CodexAdapterError> {
        let thread_id = bounded(thread_id.to_owned(), 4096)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(CodexAdapterError::InvalidMessage(
                "turn prompt must not be empty".into(),
            ));
        }
        let prompt = bounded(prompt.to_owned(), 16 * 1024)?;
        self.process.begin_effect()?;
        let response = self.request_raw(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{
                    "type": "text",
                    "text": prompt,
                    "text_elements": []
                }]
            }),
            timeout,
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if self.process.state()? == DriverState::Busy {
                    self.process.finish_effect()?;
                }
                return Err(error);
            }
        };
        response
            .pointer("/result/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodexAdapterError::InvalidMessage("turn/start returned no turn id".into())
            })
            .and_then(|value| bounded(value.to_owned(), 4096))
    }

    pub fn resolve_effect(
        &self,
        request_id: &ExternalRequestId,
        authorization: AgentEffectAuthorization,
    ) -> Result<(), CodexAdapterError> {
        if authorization.operation_revision == 0 {
            return Err(CodexAdapterError::InvalidAuthorization(
                "operation revision must be positive".into(),
            ));
        }
        let proposal = lock(&self.pending)?
            .get(request_id)
            .cloned()
            .ok_or(CodexAdapterError::UnknownApproval)?;
        if authorization.proposal_sha256 != proposal.payload_sha256 {
            return Err(CodexAdapterError::InvalidAuthorization(
                "proposal digest does not match the pending request".into(),
            ));
        }
        let decision = match authorization.decision {
            PermissionDecision::AllowOnce => "accept",
            PermissionDecision::RejectOnce => "decline",
            PermissionDecision::Cancelled => "cancel",
            PermissionDecision::AllowAlways | PermissionDecision::RejectAlways => {
                return Err(CodexAdapterError::InvalidAuthorization(
                    "persistent policy decisions are not wire-level approvals".into(),
                ));
            }
        };
        if authorization.decision == PermissionDecision::AllowOnce {
            match self.process.state()? {
                DriverState::Ready => self.process.begin_effect()?,
                DriverState::Busy => {}
                state => return Err(CodexAdapterError::Exited(state)),
            }
        }
        self.process.send_json(&json!({
            "id": request_id_value(request_id),
            "result": {"decision": decision}
        }))?;
        lock(&self.pending)?.remove(request_id);
        Ok(())
    }

    pub fn pending_effects(&self) -> Result<Vec<AgentEffectProposal>, CodexAdapterError> {
        Ok(lock(&self.pending)?.values().cloned().collect())
    }

    pub fn state(&self) -> Result<DriverState, CodexAdapterError> {
        Ok(self.process.state()?)
    }

    pub fn stderr_tail(&self) -> Result<String, CodexAdapterError> {
        Ok(self.process.stderr_tail()?)
    }

    pub fn close(&self) -> Result<DriverState, CodexAdapterError> {
        let state = self.process.stop(Duration::from_millis(250))?;
        remove_staged_auth_file(self.staged_auth_file.as_ref());
        Ok(state)
    }

    fn request_raw(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CodexAdapterError> {
        let _gate = lock(&self.request_gate)?;
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.process.send_json(&json!({
            "id": id,
            "method": method,
            "params": params
        }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CodexAdapterError::Timeout { request_id: id });
            }
            let event = self.process.recv_timeout(remaining)?;
            match event {
                DriverEvent::Message { ref payload, .. }
                    if payload.get("id") == Some(&Value::from(id)) =>
                {
                    if let Some(error) = payload.get("error") {
                        return Err(CodexAdapterError::Remote {
                            request_id: id,
                            error: error.clone(),
                        });
                    }
                    return Ok(message_payload(event));
                }
                DriverEvent::ProtocolError { ref message } => {
                    return Err(CodexAdapterError::Protocol(message.clone()));
                }
                DriverEvent::Exited { state, .. } => {
                    return Err(CodexAdapterError::Exited(state));
                }
                event => {
                    let mut inbox = lock(&self.inbox)?;
                    if inbox.len() == MAX_BUFFERED_MESSAGES {
                        return Err(CodexAdapterError::InboxOverflow);
                    }
                    inbox.push_back(event);
                }
            }
        }
    }

    fn normalize_message(
        &self,
        sequence: u64,
        payload: Value,
    ) -> Result<AgentDriverEvent, CodexAdapterError> {
        let method = payload.get("method").and_then(Value::as_str);
        if let Some(
            method @ ("item/commandExecution/requestApproval" | "item/fileChange/requestApproval"),
        ) = method
        {
            let request_id = external_request_id(payload.get("id"))?;
            let proposal = normalize_effect(
                self.process.manifest().driver_id,
                request_id.clone(),
                method,
                payload.get("params").cloned().unwrap_or(Value::Null),
            )?;
            let mut pending = lock(&self.pending)?;
            if pending.len() == MAX_PENDING_APPROVALS {
                return Err(CodexAdapterError::ApprovalOverflow);
            }
            if pending.insert(request_id, proposal.clone()).is_some() {
                return Err(CodexAdapterError::DuplicateApproval);
            }
            return Ok(AgentDriverEvent::EffectProposed { sequence, proposal });
        }
        if payload.get("id").is_some() && method.is_some() {
            self.process.send_json(&json!({
                "id": payload["id"].clone(),
                "error": {"code": -32601, "message": "unsupported by Hyper Term"}
            }))?;
        }
        let params = payload.get("params").unwrap_or(&Value::Null);
        match method {
            Some("item/agentMessage/delta") => Ok(AgentDriverEvent::MessageDelta {
                sequence,
                thread_id: required_string(params, "threadId")?,
                turn_id: required_string(params, "turnId")?,
                text: required_string(params, "delta")?,
            }),
            Some("item/plan/delta") => Ok(AgentDriverEvent::PlanDelta {
                sequence,
                thread_id: required_string(params, "threadId")?,
                turn_id: required_string(params, "turnId")?,
                text: required_string(params, "delta")?,
            }),
            Some("turn/completed") => {
                if self.process.state()? == DriverState::Busy {
                    self.process.finish_effect()?;
                }
                Ok(AgentDriverEvent::TurnCompleted {
                    sequence,
                    thread_id: required_string(params, "threadId")?,
                    turn_id: params
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    status: params
                        .pointer("/turn/status")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                })
            }
            _ => Ok(AgentDriverEvent::ProtocolNotice {
                sequence,
                method: method.map(ToOwned::to_owned),
                payload_sha256: sha256_value(&payload)?,
            }),
        }
    }
}

impl Drop for CodexAppServerClient {
    fn drop(&mut self) {
        remove_staged_auth_file(self.staged_auth_file.as_ref());
    }
}

#[cfg(unix)]
fn stage_auth_file(
    source: Option<&PathBuf>,
    codex_home: &std::path::Path,
) -> Result<Option<PathBuf>, CodexAdapterError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    let Some(source) = source else {
        return Ok(None);
    };
    let source = source.canonicalize()?;
    let metadata = fs::metadata(&source)?;
    if !metadata.is_file() {
        return Err(CodexAdapterError::InvalidConfig(
            "Codex auth source is not a regular file".into(),
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(CodexAdapterError::InvalidConfig(
            "Codex auth source is not owned by the current user".into(),
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(CodexAdapterError::InvalidConfig(
            "Codex auth source must not be accessible by group or other users".into(),
        ));
    }
    let target = codex_home.join("auth.json");
    if let Ok(existing) = fs::symlink_metadata(&target) {
        if !existing.file_type().is_symlink() {
            return Err(CodexAdapterError::InvalidConfig(
                "isolated Codex home already contains a non-symlink auth.json".into(),
            ));
        }
        fs::remove_file(&target)?;
    }
    symlink(source, &target)?;
    Ok(Some(target))
}

#[cfg(not(unix))]
fn stage_auth_file(
    source: Option<&PathBuf>,
    _codex_home: &std::path::Path,
) -> Result<Option<PathBuf>, CodexAdapterError> {
    if source.is_some() {
        return Err(CodexAdapterError::InvalidConfig(
            "Codex auth staging currently requires Unix".into(),
        ));
    }
    Ok(None)
}

fn remove_staged_auth_file(path: Option<&PathBuf>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

fn codex_arguments(
    mcp_server: Option<&CodexMcpServerConfig>,
) -> Result<Vec<OsString>, CodexAdapterError> {
    let mut arguments = vec![
        OsString::from("app-server"),
        OsString::from("--stdio"),
        OsString::from("--strict-config"),
    ];
    let Some(mcp_server) = mcp_server else {
        return Ok(arguments);
    };
    if !mcp_server.executable.is_absolute() {
        return Err(CodexAdapterError::InvalidConfig(
            "brokered MCP executable must be absolute".into(),
        ));
    }
    if mcp_server.arguments.len() > 32 {
        return Err(CodexAdapterError::InvalidConfig(
            "brokered MCP arguments exceed their bound".into(),
        ));
    }
    let executable = mcp_server.executable.canonicalize()?;
    let actual_digest = sha256_file(&executable)?;
    if actual_digest != mcp_server.executable_sha256 {
        return Err(CodexAdapterError::McpExecutableDigestMismatch {
            expected: mcp_server.executable_sha256.clone(),
            actual: actual_digest,
        });
    }
    let executable = executable.to_str().ok_or_else(|| {
        CodexAdapterError::InvalidConfig("brokered MCP executable path is not UTF-8".into())
    })?;
    let mcp_arguments = mcp_server
        .arguments
        .iter()
        .map(|argument| {
            let value = argument.to_str().ok_or_else(|| {
                CodexAdapterError::InvalidConfig("brokered MCP argument is not UTF-8".into())
            })?;
            if value.len() > 16 * 1024 {
                return Err(CodexAdapterError::InvalidConfig(
                    "brokered MCP argument exceeds its bound".into(),
                ));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, CodexAdapterError>>()?;
    let executable = serde_json::to_string(executable)?;
    let mcp_arguments = serde_json::to_string(&mcp_arguments)?;
    let enabled_tools = serde_json::to_string(HYPER_TERM_MCP_TOOLS)?;
    for value in [
        format!("mcp_servers.hyper_term.command={executable}"),
        format!("mcp_servers.hyper_term.args={mcp_arguments}"),
        format!("mcp_servers.hyper_term.enabled_tools={enabled_tools}"),
        "mcp_servers.hyper_term.startup_timeout_sec=5".into(),
        "mcp_servers.hyper_term.tool_timeout_sec=30".into(),
    ] {
        arguments.push(OsString::from("-c"));
        arguments.push(OsString::from(value));
    }
    Ok(arguments)
}

fn normalize_effect(
    driver_id: Uuid,
    request_id: ExternalRequestId,
    method: &str,
    params: Value,
) -> Result<AgentEffectProposal, CodexAdapterError> {
    let command = params.get("command").and_then(Value::as_str);
    let reason = params.get("reason").and_then(Value::as_str);
    let (kind, summary, mut required_capabilities) = match method {
        "item/commandExecution/requestApproval" => (
            AgentEffectKind::Shell,
            command
                .unwrap_or("Codex requested command execution")
                .to_owned(),
            vec!["shell".into()],
        ),
        "item/fileChange/requestApproval" => (
            AgentEffectKind::WorkspaceEdit,
            reason
                .unwrap_or("Codex requested a workspace change")
                .to_owned(),
            vec!["workspace_write".into()],
        ),
        _ => return Err(CodexAdapterError::UnsupportedApproval(method.into())),
    };
    if !params["networkApprovalContext"].is_null() {
        required_capabilities.push("network".into());
    }
    Ok(AgentEffectProposal {
        driver_id,
        protocol: StructuredAgentProtocol::CodexAppServerV2,
        request_id,
        method: method.into(),
        kind,
        summary: bounded(summary, 16 * 1024)?,
        required_capabilities,
        payload_sha256: sha256_value(&params)?,
        thread_id: optional_bounded_string(&params, "threadId")?,
        turn_id: optional_bounded_string(&params, "turnId")?,
        item_id: optional_bounded_string(&params, "itemId")?,
    })
}

fn external_request_id(value: Option<&Value>) -> Result<ExternalRequestId, CodexAdapterError> {
    match value {
        Some(Value::String(value)) => Ok(ExternalRequestId::String(bounded(value.clone(), 512)?)),
        Some(Value::Number(value)) if value.is_i64() => {
            Ok(ExternalRequestId::Signed(value.as_i64().unwrap()))
        }
        Some(Value::Number(value)) if value.is_u64() => {
            Ok(ExternalRequestId::Unsigned(value.as_u64().unwrap()))
        }
        _ => Err(CodexAdapterError::InvalidMessage(
            "server request has no supported ID".into(),
        )),
    }
}

fn request_id_value(id: &ExternalRequestId) -> Value {
    match id {
        ExternalRequestId::String(value) => Value::String(value.clone()),
        ExternalRequestId::Signed(value) => Value::from(*value),
        ExternalRequestId::Unsigned(value) => Value::from(*value),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, CodexAdapterError> {
    let value = params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CodexAdapterError::InvalidMessage(format!("missing {key}")))?;
    bounded(value.to_owned(), 64 * 1024)
}

fn optional_bounded_string(params: &Value, key: &str) -> Result<Option<String>, CodexAdapterError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(|value| bounded(value.to_owned(), 4096))
        .transpose()
}

fn bounded(value: String, maximum: usize) -> Result<String, CodexAdapterError> {
    if value.len() > maximum {
        Err(CodexAdapterError::InvalidMessage(format!(
            "text exceeds {maximum} bytes"
        )))
    } else {
        Ok(value)
    }
}

fn sha256_value(value: &Value) -> Result<String, CodexAdapterError> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn message_payload(event: DriverEvent) -> Value {
    match event {
        DriverEvent::Message { payload, .. } => payload,
        _ => unreachable!("caller selects message events"),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, CodexAdapterError> {
    mutex.lock().map_err(|_| CodexAdapterError::LockPoisoned)
}

#[derive(Debug, Error)]
pub enum CodexAdapterError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("Codex adapter filesystem setup failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Codex adapter JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid Codex adapter configuration: {0}")]
    InvalidConfig(String),
    #[error("brokered MCP executable digest mismatch: expected {expected}, got {actual}")]
    McpExecutableDigestMismatch { expected: String, actual: String },
    #[error("invalid Codex message: {0}")]
    InvalidMessage(String),
    #[error("Codex request {request_id} timed out")]
    Timeout { request_id: u64 },
    #[error("Codex request {request_id} failed: {error}")]
    Remote { request_id: u64, error: Value },
    #[error("Codex protocol failed: {0}")]
    Protocol(String),
    #[error("Codex driver exited in state {0:?}")]
    Exited(DriverState),
    #[error("Codex message inbox exceeded its bound")]
    InboxOverflow,
    #[error("Codex pending approvals exceeded their bound")]
    ApprovalOverflow,
    #[error("Codex repeated an approval request ID")]
    DuplicateApproval,
    #[error("Codex approval request is no longer pending")]
    UnknownApproval,
    #[error("unsupported Codex approval request: {0}")]
    UnsupportedApproval(String),
    #[error("invalid operation authorization: {0}")]
    InvalidAuthorization(String),
    #[error("Codex adapter lock was poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;

    #[test]
    fn command_approval_becomes_a_bounded_effect_proposal() {
        let proposal = normalize_effect(
            Uuid::nil(),
            ExternalRequestId::Unsigned(7),
            "item/commandExecution/requestApproval",
            json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "command": "cargo test --workspace",
                "networkApprovalContext": {"host": "crates.io"}
            }),
        )
        .unwrap();
        assert_eq!(proposal.kind, AgentEffectKind::Shell);
        assert_eq!(proposal.summary, "cargo test --workspace");
        assert_eq!(proposal.required_capabilities, vec!["shell", "network"]);
        assert_eq!(proposal.payload_sha256.len(), 64);
    }

    #[test]
    fn agent_delta_preserves_semantic_ids() {
        let params = json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "item-1",
            "delta": "working"
        });
        assert_eq!(required_string(&params, "threadId").unwrap(), "thread-1");
        assert_eq!(required_string(&params, "delta").unwrap(), "working");
    }

    #[test]
    fn brokered_mcp_is_a_private_codex_launch_override() {
        let executable = Path::new("/usr/bin/true").canonicalize().unwrap();
        let arguments = codex_arguments(Some(&CodexMcpServerConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            arguments: vec![
                OsString::from("mcp-stdio"),
                OsString::from("--agent-socket=/tmp/hyper-term-agent.sock"),
            ],
        }))
        .unwrap();
        let arguments = arguments
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            &arguments[..3],
            ["app-server", "--stdio", "--strict-config"]
        );
        assert!(arguments.contains(&"mcp_servers.hyper_term.command=\"/usr/bin/true\""));
        assert!(arguments.iter().any(|value| {
            value.starts_with("mcp_servers.hyper_term.args=")
                && value.contains("hyper-term-agent.sock")
        }));
        assert!(arguments.iter().any(|value| {
            value.starts_with("mcp_servers.hyper_term.enabled_tools=")
                && HYPER_TERM_MCP_TOOLS.iter().all(|tool| value.contains(tool))
        }));
    }

    #[test]
    fn brokered_mcp_binary_is_digest_pinned_before_codex_sees_it() {
        let executable = Path::new("/usr/bin/true").canonicalize().unwrap();
        let result = codex_arguments(Some(&CodexMcpServerConfig {
            executable,
            executable_sha256: "0".repeat(64),
            arguments: vec![],
        }));
        assert!(matches!(
            result,
            Err(CodexAdapterError::McpExecutableDigestMismatch { .. })
        ));
    }

    #[test]
    fn isolated_auth_staging_accepts_only_private_user_credentials() {
        let temporary = tempfile::tempdir().unwrap();
        let codex_home = temporary.path().join("codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        let auth = temporary.path().join("auth.json");
        fs::write(&auth, "{}").unwrap();

        let mut permissions = fs::metadata(&auth).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&auth, permissions).unwrap();
        assert!(matches!(
            stage_auth_file(Some(&auth), &codex_home),
            Err(CodexAdapterError::InvalidConfig(_))
        ));

        let mut permissions = fs::metadata(&auth).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&auth, permissions).unwrap();
        let staged = stage_auth_file(Some(&auth), &codex_home)
            .unwrap()
            .expect("staged auth");
        assert!(
            fs::symlink_metadata(&staged)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::canonicalize(&staged).unwrap(),
            fs::canonicalize(&auth).unwrap()
        );
        remove_staged_auth_file(Some(&staged));
        assert!(!staged.exists());
    }
}
