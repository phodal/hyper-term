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
    ExternalRequestId, StructuredAgentProtocol,
};

const MAX_PENDING_APPROVALS: usize = 128;
const MAX_BUFFERED_MESSAGES: usize = 512;
const CODEX_FRAME_BYTES: usize = 2 * 1024 * 1024;

pub struct CodexAppServerConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub implementation_version: String,
    pub workspace: PathBuf,
    pub codex_home: PathBuf,
    pub scratch_directory: PathBuf,
}

pub struct CodexAppServerClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<VecDeque<DriverEvent>>,
    pending: Mutex<HashMap<ExternalRequestId, AgentEffectProposal>>,
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
        let codex_home = config.codex_home.canonicalize()?;
        let scratch = config.scratch_directory.canonicalize()?;
        let environment = BTreeMap::from([
            ("CODEX_HOME".into(), codex_home.into_os_string()),
            ("HOME".into(), scratch.clone().into_os_string()),
            ("NO_COLOR".into(), OsString::from("1")),
            ("RUST_BACKTRACE".into(), OsString::from("0")),
            ("TERM".into(), OsString::from("dumb")),
            ("TMPDIR".into(), scratch.into_os_string()),
        ]);
        let process = DriverProcess::spawn(DriverSpec {
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
            arguments: vec![
                OsString::from("app-server"),
                OsString::from("--stdio"),
                OsString::from("--strict-config"),
            ],
            working_directory: workspace,
            environment,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: CODEX_FRAME_BYTES,
        })?;
        Ok(Self {
            process,
            next_request_id: AtomicU64::new(1),
            request_gate: Mutex::new(()),
            inbox: Mutex::new(VecDeque::new()),
            pending: Mutex::new(HashMap::new()),
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
        Ok(self.process.stop(Duration::from_millis(250))?)
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
}
