use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agent_client_protocol::JsonRpcMessage;
use agent_client_protocol::schema::{ProtocolVersion, v1};
use hyper_term_protocol::PermissionDecision;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    AgentClientError, AgentDriverEvent, AgentEffectAuthorization, AgentEffectKind,
    AgentEffectProposal, DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES, DriverError, DriverEvent,
    DriverFraming, DriverKind, DriverManifest, DriverProcess, DriverSpec, DriverState,
    ExternalRequestId, StructuredAgentClient, StructuredAgentProtocol, process::BoundedDriverInbox,
    sha256_file,
};

const ACP_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUFFERED_MESSAGES: usize = 512;
const MAX_BUFFERED_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_APPROVALS: usize = 128;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const HYPER_TERM_MCP_TOOLS: &[&str] = &[
    "hyper_term.genui.compile",
    "hyper_term.lsp.query",
    "hyper_term.diff.review",
];

#[derive(Clone)]
pub struct AcpMcpServerConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub arguments: Vec<OsString>,
}

impl StructuredAgentClient for AcpAgentClient {
    fn provider_id(&self) -> &str {
        AcpAgentClient::provider_id(self)
    }

    fn protocol(&self) -> StructuredAgentProtocol {
        StructuredAgentProtocol::Acp
    }

    fn initialize_session(&self, timeout: Duration) -> Result<String, AgentClientError> {
        AcpAgentClient::initialize(self, timeout)?;
        Ok(AcpAgentClient::start_session(self, timeout)?)
    }

    fn start_turn(
        &self,
        session_id: &str,
        prompt: &str,
        _timeout: Duration,
    ) -> Result<String, AgentClientError> {
        Ok(AcpAgentClient::start_turn(self, session_id, prompt)?)
    }

    fn next_event(&self, timeout: Duration) -> Result<AgentDriverEvent, AgentClientError> {
        Ok(AcpAgentClient::next_event(self, timeout)?)
    }

    fn resolve_effect(
        &self,
        request_id: &ExternalRequestId,
        authorization: AgentEffectAuthorization,
    ) -> Result<(), AgentClientError> {
        Ok(AcpAgentClient::resolve_effect(
            self,
            request_id,
            authorization,
        )?)
    }

    fn state(&self) -> Result<DriverState, AgentClientError> {
        Ok(AcpAgentClient::state(self)?)
    }

    fn stderr_tail(&self) -> Result<String, AgentClientError> {
        Ok(AcpAgentClient::stderr_tail(self)?)
    }

    fn close(&self) -> Result<DriverState, AgentClientError> {
        Ok(AcpAgentClient::close(self)?)
    }
}

pub struct AcpAgentConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<String, OsString>,
    pub implementation_version: String,
    pub provider_id: String,
    pub workspace: PathBuf,
    pub brokered_mcp_server: Option<AcpMcpServerConfig>,
}

pub struct AcpAgentClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<BoundedDriverInbox>,
    pending_prompt: Mutex<Option<PendingPrompt>>,
    pending_approvals: Mutex<HashMap<ExternalRequestId, PendingApproval>>,
    brokered_mcp_calls: Mutex<HashMap<String, BrokeredMcpCall>>,
    provider_id: String,
    workspace: PathBuf,
    mcp_servers: Vec<v1::McpServer>,
}

#[derive(Clone)]
struct PendingPrompt {
    request_id: u64,
    session_id: String,
    turn_id: String,
}

#[derive(Clone)]
struct PendingApproval {
    proposal: AgentEffectProposal,
    options: Vec<v1::PermissionOption>,
}

#[derive(Clone)]
struct BrokeredMcpCall {
    session_id: String,
    tool_name: String,
}

impl AcpAgentClient {
    pub fn launch(config: AcpAgentConfig) -> Result<Self, AcpAdapterError> {
        validate_provider_id(&config.provider_id)?;
        if !config.workspace.is_absolute() {
            return Err(AcpAdapterError::InvalidConfig(
                "ACP workspace must be absolute".into(),
            ));
        }
        let workspace = config.workspace.canonicalize()?;
        let mcp_servers = config
            .brokered_mcp_server
            .as_ref()
            .map(acp_mcp_server)
            .transpose()?
            .into_iter()
            .collect();
        let process = DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id: Uuid::new_v4(),
                kind: DriverKind::AcpAgent,
                implementation_version: config.implementation_version,
                protocol_version: "acp-v1".into(),
                capabilities: vec![
                    "sessions".into(),
                    "streaming".into(),
                    "plans".into(),
                    "permission_proposals".into(),
                    "mcp_stdio".into(),
                ],
                transport: "stdio-jsonrpc-jsonl".into(),
                executable_sha256: config.executable_sha256,
                permission_profile: "acp-proposal-only-v1".into(),
            },
            executable: config.executable,
            arguments: config.arguments,
            working_directory: workspace.clone(),
            environment: config.environment,
            sandbox: None,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: ACP_FRAME_BYTES,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })?;
        Ok(Self {
            process,
            next_request_id: AtomicU64::new(1),
            request_gate: Mutex::new(()),
            inbox: Mutex::new(BoundedDriverInbox::new(
                MAX_BUFFERED_MESSAGES,
                MAX_BUFFERED_MESSAGE_BYTES,
            )),
            pending_prompt: Mutex::new(None),
            pending_approvals: Mutex::new(HashMap::new()),
            brokered_mcp_calls: Mutex::new(HashMap::new()),
            provider_id: config.provider_id,
            workspace,
            mcp_servers,
        })
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn initialize(&self, timeout: Duration) -> Result<v1::InitializeResponse, AcpAdapterError> {
        let request = v1::InitializeRequest::new(ProtocolVersion::V1).client_info(
            v1::Implementation::new("hyper-term", env!("CARGO_PKG_VERSION")).title("Hyper Term"),
        );
        let response = self.request(&request, timeout)?;
        if response.protocol_version != ProtocolVersion::V1 {
            return Err(AcpAdapterError::UnsupportedProtocol);
        }
        self.process.mark_ready()?;
        Ok(response)
    }

    pub fn start_session(&self, timeout: Duration) -> Result<String, AcpAdapterError> {
        let request = v1::NewSessionRequest::new(self.workspace.clone())
            .mcp_servers(self.mcp_servers.clone());
        let response: v1::NewSessionResponse = self.request(&request, timeout)?;
        bounded(response.session_id.to_string(), 4096)
    }

    pub fn start_turn(&self, session_id: &str, prompt: &str) -> Result<String, AcpAdapterError> {
        let session_id = bounded(session_id.to_owned(), 4096)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(AcpAdapterError::InvalidMessage(
                "turn prompt must not be empty".into(),
            ));
        }
        let prompt = bounded(prompt.to_owned(), 16 * 1024)?;
        let _gate = lock(&self.request_gate)?;
        let mut pending = lock(&self.pending_prompt)?;
        if pending.is_some() {
            return Err(AcpAdapterError::PromptAlreadyRunning);
        }
        lock(&self.brokered_mcp_calls)?.clear();
        self.process.begin_effect()?;
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let turn_id = format!("acp-turn-{request_id}");
        let request = v1::PromptRequest::new(session_id.clone(), vec![prompt.into()]);
        let message = json_rpc_request(request_id, &request)?;
        *pending = Some(PendingPrompt {
            request_id,
            session_id,
            turn_id: turn_id.clone(),
        });
        if let Err(error) = self.process.send_json(&message) {
            pending.take();
            let _ = self.process.finish_effect();
            return Err(error.into());
        }
        Ok(turn_id)
    }

    pub fn next_event(&self, timeout: Duration) -> Result<AgentDriverEvent, AcpAdapterError> {
        let event = if let Some(event) = lock(&self.inbox)?.pop_front() {
            event
        } else {
            self.process.recv_timeout(timeout)?
        };
        match event {
            DriverEvent::Message { sequence, payload } => self.normalize_message(sequence, payload),
            DriverEvent::ProtocolError { message } => Err(AcpAdapterError::Protocol(message)),
            DriverEvent::Exited { code, state } => Ok(AgentDriverEvent::Exited { code, state }),
        }
    }

    pub fn resolve_effect(
        &self,
        request_id: &ExternalRequestId,
        authorization: AgentEffectAuthorization,
    ) -> Result<(), AcpAdapterError> {
        if authorization.operation_revision == 0 {
            return Err(AcpAdapterError::InvalidAuthorization(
                "operation revision must be positive".into(),
            ));
        }
        let approval = lock(&self.pending_approvals)?
            .get(request_id)
            .cloned()
            .ok_or(AcpAdapterError::UnknownApproval)?;
        if authorization.proposal_sha256 != approval.proposal.payload_sha256 {
            return Err(AcpAdapterError::InvalidAuthorization(
                "proposal digest does not match the pending request".into(),
            ));
        }
        let outcome = match authorization.decision {
            PermissionDecision::Cancelled => v1::RequestPermissionOutcome::Cancelled,
            PermissionDecision::AllowOnce | PermissionDecision::RejectOnce => {
                let wanted = match authorization.decision {
                    PermissionDecision::AllowOnce => v1::PermissionOptionKind::AllowOnce,
                    PermissionDecision::RejectOnce => v1::PermissionOptionKind::RejectOnce,
                    _ => unreachable!(),
                };
                let option = approval
                    .options
                    .iter()
                    .find(|option| option.kind == wanted)
                    .ok_or(AcpAdapterError::DecisionUnavailable)?;
                v1::RequestPermissionOutcome::Selected(v1::SelectedPermissionOutcome::new(
                    option.option_id.clone(),
                ))
            }
            PermissionDecision::AllowAlways | PermissionDecision::RejectAlways => {
                return Err(AcpAdapterError::InvalidAuthorization(
                    "persistent policy decisions are not wire-level approvals".into(),
                ));
            }
        };
        let response = v1::RequestPermissionResponse::new(outcome);
        self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "id": request_id_value(request_id),
            "result": response,
        }))?;
        lock(&self.pending_approvals)?.remove(request_id);
        Ok(())
    }

    pub fn pending_effects(&self) -> Result<Vec<AgentEffectProposal>, AcpAdapterError> {
        Ok(lock(&self.pending_approvals)?
            .values()
            .map(|approval| approval.proposal.clone())
            .collect())
    }

    pub fn state(&self) -> Result<DriverState, AcpAdapterError> {
        Ok(self.process.state()?)
    }

    pub fn stderr_tail(&self) -> Result<String, AcpAdapterError> {
        Ok(self.process.stderr_tail()?)
    }

    pub fn close(&self) -> Result<DriverState, AcpAdapterError> {
        Ok(self.process.stop(Duration::from_millis(250))?)
    }

    fn request<Request>(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Request::Response, AcpAdapterError>
    where
        Request: agent_client_protocol::JsonRpcRequest + Serialize,
        Request::Response: DeserializeOwned,
    {
        let _gate = lock(&self.request_gate)?;
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let response = self.request_raw(id, request, timeout)?;
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| AcpAdapterError::InvalidMessage("ACP response has no result".into()))?;
        Ok(serde_json::from_value(result)?)
    }

    fn request_raw(
        &self,
        id: u64,
        request: &(impl JsonRpcMessage + Serialize),
        timeout: Duration,
    ) -> Result<Value, AcpAdapterError> {
        self.process.send_json(&json_rpc_request(id, request)?)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AcpAdapterError::Timeout { request_id: id });
            }
            let event = self.process.recv_timeout(remaining)?;
            match event {
                DriverEvent::Message { ref payload, .. }
                    if payload.get("id") == Some(&Value::from(id)) =>
                {
                    if let Some(error) = payload.get("error") {
                        return Err(AcpAdapterError::Remote {
                            request_id: id,
                            error: error.clone(),
                        });
                    }
                    return Ok(message_payload(event));
                }
                DriverEvent::Message { ref payload, .. }
                    if payload.get("id").is_some() && payload.get("method").is_some() =>
                {
                    reject_unsupported_request(&self.process, payload)?;
                }
                DriverEvent::ProtocolError { ref message } => {
                    return Err(AcpAdapterError::Protocol(message.clone()));
                }
                DriverEvent::Exited { state, .. } => return Err(AcpAdapterError::Exited(state)),
                event => lock(&self.inbox)?
                    .push_back(event)
                    .map_err(|_| AcpAdapterError::InboxOverflow)?,
            }
        }
    }

    fn normalize_message(
        &self,
        sequence: u64,
        payload: Value,
    ) -> Result<AgentDriverEvent, AcpAdapterError> {
        if is_pending_prompt_response(&payload, &*lock(&self.pending_prompt)?) {
            return self.complete_prompt(sequence, payload);
        }
        let method = payload.get("method").and_then(Value::as_str);
        match method {
            Some("session/update") => {
                let notification: v1::SessionNotification =
                    parse_params(&payload, "session/update")?;
                self.normalize_session_update(sequence, notification)
            }
            Some("session/request_permission") => {
                self.normalize_permission_request(sequence, payload)
            }
            Some(_) if payload.get("id").is_some() => {
                reject_unsupported_request(&self.process, &payload)?;
                protocol_notice(sequence, method, &payload)
            }
            _ => protocol_notice(sequence, method, &payload),
        }
    }

    fn normalize_session_update(
        &self,
        sequence: u64,
        notification: v1::SessionNotification,
    ) -> Result<AgentDriverEvent, AcpAdapterError> {
        let thread_id = bounded(notification.session_id.to_string(), 4096)?;
        let turn_id = lock(&self.pending_prompt)?
            .as_ref()
            .filter(|pending| pending.session_id == thread_id)
            .map(|pending| pending.turn_id.clone())
            .unwrap_or_else(|| "acp-turn-unknown".into());
        self.track_brokered_mcp_update(&thread_id, &notification.update)?;
        match notification.update {
            v1::SessionUpdate::AgentMessageChunk(chunk) => Ok(AgentDriverEvent::MessageDelta {
                sequence,
                thread_id,
                turn_id,
                text: content_text(chunk.content)?,
            }),
            v1::SessionUpdate::AgentThoughtChunk(chunk) => Ok(AgentDriverEvent::ThoughtDelta {
                sequence,
                thread_id,
                turn_id,
                text: content_text(chunk.content)?,
            }),
            v1::SessionUpdate::Plan(plan) => Ok(AgentDriverEvent::PlanDelta {
                sequence,
                thread_id,
                turn_id,
                text: bounded(serde_json::to_string(&plan)?, 64 * 1024)?,
            }),
            update => protocol_notice(
                sequence,
                Some("session/update"),
                &serde_json::to_value(update)?,
            ),
        }
    }

    fn normalize_permission_request(
        &self,
        sequence: u64,
        payload: Value,
    ) -> Result<AgentDriverEvent, AcpAdapterError> {
        let request_id = external_request_id(payload.get("id"))?;
        let params = payload.get("params").cloned().unwrap_or(Value::Null);
        let request: v1::RequestPermissionRequest = serde_json::from_value(params.clone())?;
        if self.resolve_brokered_mcp_consent(&request_id, &request)? {
            return protocol_notice(sequence, Some("session/request_permission"), &payload);
        }
        let proposal = normalize_permission(
            self.process.manifest().driver_id,
            request_id.clone(),
            &request,
            &params,
            lock(&self.pending_prompt)?.as_ref(),
        )?;
        let mut pending = lock(&self.pending_approvals)?;
        if pending.len() == MAX_PENDING_APPROVALS {
            return Err(AcpAdapterError::ApprovalOverflow);
        }
        if pending
            .insert(
                request_id,
                PendingApproval {
                    proposal: proposal.clone(),
                    options: request.options,
                },
            )
            .is_some()
        {
            return Err(AcpAdapterError::DuplicateApproval);
        }
        Ok(AgentDriverEvent::EffectProposed { sequence, proposal })
    }

    fn track_brokered_mcp_update(
        &self,
        session_id: &str,
        update: &v1::SessionUpdate,
    ) -> Result<(), AcpAdapterError> {
        let mut calls = lock(&self.brokered_mcp_calls)?;
        match update {
            v1::SessionUpdate::ToolCall(call) => {
                let Some(tool_name) = brokered_mcp_tool(call) else {
                    return Ok(());
                };
                let tool_call_id = bounded(call.tool_call_id.to_string(), 4096)?;
                if calls.len() < MAX_PENDING_APPROVALS || calls.contains_key(&tool_call_id) {
                    calls.insert(
                        tool_call_id,
                        BrokeredMcpCall {
                            session_id: session_id.to_owned(),
                            tool_name: tool_name.to_owned(),
                        },
                    );
                }
            }
            v1::SessionUpdate::ToolCallUpdate(call)
                if matches!(
                    call.fields.status,
                    Some(v1::ToolCallStatus::Completed | v1::ToolCallStatus::Failed)
                ) =>
            {
                calls.remove(call.tool_call_id.to_string().as_str());
            }
            _ => {}
        }
        Ok(())
    }

    fn resolve_brokered_mcp_consent(
        &self,
        request_id: &ExternalRequestId,
        request: &v1::RequestPermissionRequest,
    ) -> Result<bool, AcpAdapterError> {
        // Codex ACP asks its client for transport-level consent before it sends
        // the correlated tools/call to the configured MCP server. Forward that
        // consent only for a previously observed, allowlisted Hyper Term tool.
        // The digest-pinned MCP server still validates the exact arguments and
        // creates the user-visible Rust broker operation before executing it.
        if self.mcp_servers.is_empty() || !is_brokered_mcp_consent(request) {
            return Ok(false);
        }
        let tool_call_id = request.tool_call.tool_call_id.to_string();
        let Some(call) = lock(&self.brokered_mcp_calls)?.get(&tool_call_id).cloned() else {
            return Ok(false);
        };
        if call.session_id != request.session_id.to_string()
            || !HYPER_TERM_MCP_TOOLS.contains(&call.tool_name.as_str())
        {
            return Ok(false);
        }
        let Some(option) = request
            .options
            .iter()
            .find(|option| option.kind == v1::PermissionOptionKind::AllowOnce)
        else {
            return Ok(false);
        };
        let response = v1::RequestPermissionResponse::new(v1::RequestPermissionOutcome::Selected(
            v1::SelectedPermissionOutcome::new(option.option_id.clone()),
        ));
        self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "id": request_id_value(request_id),
            "result": response,
        }))?;
        lock(&self.brokered_mcp_calls)?.remove(&tool_call_id);
        Ok(true)
    }

    fn complete_prompt(
        &self,
        sequence: u64,
        payload: Value,
    ) -> Result<AgentDriverEvent, AcpAdapterError> {
        let pending = lock(&self.pending_prompt)?.take().ok_or_else(|| {
            AcpAdapterError::InvalidMessage("ACP prompt response is stale".into())
        })?;
        self.process.finish_effect()?;
        if let Some(error) = payload.get("error") {
            return Err(AcpAdapterError::Remote {
                request_id: pending.request_id,
                error: error.clone(),
            });
        }
        let response: v1::PromptResponse =
            serde_json::from_value(payload.get("result").cloned().ok_or_else(|| {
                AcpAdapterError::InvalidMessage("ACP prompt has no result".into())
            })?)?;
        let status = serde_json::to_value(response.stop_reason)?
            .as_str()
            .map(ToOwned::to_owned);
        Ok(AgentDriverEvent::TurnCompleted {
            sequence,
            thread_id: pending.session_id,
            turn_id: Some(pending.turn_id),
            status,
        })
    }
}

fn acp_mcp_server(config: &AcpMcpServerConfig) -> Result<v1::McpServer, AcpAdapterError> {
    if !config.executable.is_absolute() || config.arguments.len() > 32 {
        return Err(AcpAdapterError::InvalidConfig(
            "brokered MCP executable or arguments are invalid".into(),
        ));
    }
    let executable = config.executable.canonicalize()?;
    let actual = sha256_file(&executable)?;
    if actual != config.executable_sha256 {
        return Err(AcpAdapterError::McpExecutableDigestMismatch {
            expected: config.executable_sha256.clone(),
            actual,
        });
    }
    let arguments = config
        .arguments
        .iter()
        .map(|argument| {
            let value = argument.to_str().ok_or_else(|| {
                AcpAdapterError::InvalidConfig("brokered MCP argument is not UTF-8".into())
            })?;
            bounded(value.to_owned(), 16 * 1024)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v1::McpServer::Stdio(
        v1::McpServerStdio::new("hyper_term", executable).args(arguments),
    ))
}

fn normalize_permission(
    driver_id: Uuid,
    request_id: ExternalRequestId,
    request: &v1::RequestPermissionRequest,
    raw_params: &Value,
    prompt: Option<&PendingPrompt>,
) -> Result<AgentEffectProposal, AcpAdapterError> {
    let kind = match request.tool_call.fields.kind.unwrap_or_default() {
        v1::ToolKind::Execute => AgentEffectKind::Shell,
        v1::ToolKind::Edit | v1::ToolKind::Delete | v1::ToolKind::Move => {
            AgentEffectKind::WorkspaceEdit
        }
        v1::ToolKind::Fetch | v1::ToolKind::Read | v1::ToolKind::Search => AgentEffectKind::Tool,
        _ => AgentEffectKind::Opaque,
    };
    let required_capabilities = match kind {
        AgentEffectKind::Shell => vec!["shell".into()],
        AgentEffectKind::WorkspaceEdit => vec!["workspace_write".into()],
        AgentEffectKind::Tool => vec!["tool".into()],
        AgentEffectKind::ComputerUse => vec!["computer_use".into()],
        AgentEffectKind::Opaque => vec!["opaque_effect".into()],
    };
    let summary = request
        .tool_call
        .fields
        .title
        .clone()
        .unwrap_or_else(|| "ACP agent requested permission".into());
    Ok(AgentEffectProposal {
        driver_id,
        protocol: StructuredAgentProtocol::Acp,
        request_id,
        method: "session/request_permission".into(),
        kind,
        summary: bounded(summary, 16 * 1024)?,
        required_capabilities,
        payload_sha256: sha256_value(raw_params)?,
        thread_id: Some(bounded(request.session_id.to_string(), 4096)?),
        turn_id: prompt.map(|pending| pending.turn_id.clone()),
        item_id: Some(bounded(request.tool_call.tool_call_id.to_string(), 4096)?),
    })
}

fn brokered_mcp_tool(call: &v1::ToolCall) -> Option<&str> {
    if call.kind != v1::ToolKind::Execute
        || call
            .meta
            .as_ref()
            .and_then(|meta| meta.get("is_mcp_tool_call"))
            .and_then(Value::as_bool)
            != Some(true)
    {
        return None;
    }
    let input = call.raw_input.as_ref()?.as_object()?;
    if input.get("server").and_then(Value::as_str) != Some("hyper_term")
        || !input.get("arguments").is_some_and(Value::is_object)
    {
        return None;
    }
    let tool_name = input.get("tool").and_then(Value::as_str)?;
    if !HYPER_TERM_MCP_TOOLS.contains(&tool_name)
        || call.title != format!("mcp.hyper_term.{tool_name}")
    {
        return None;
    }
    Some(tool_name)
}

fn is_brokered_mcp_consent(request: &v1::RequestPermissionRequest) -> bool {
    request.tool_call.fields.kind == Some(v1::ToolKind::Execute)
        && request
            .meta
            .as_ref()
            .and_then(|meta| meta.get("is_mcp_tool_approval"))
            .and_then(Value::as_bool)
            == Some(true)
}

fn content_text(content: v1::ContentBlock) -> Result<String, AcpAdapterError> {
    match content {
        v1::ContentBlock::Text(text) => bounded(text.text, 64 * 1024),
        other => bounded(serde_json::to_string(&other)?, 64 * 1024),
    }
}

fn protocol_notice(
    sequence: u64,
    method: Option<&str>,
    payload: &Value,
) -> Result<AgentDriverEvent, AcpAdapterError> {
    Ok(AgentDriverEvent::ProtocolNotice {
        sequence,
        method: method.map(ToOwned::to_owned),
        payload_sha256: sha256_value(payload)?,
    })
}

fn json_rpc_request(
    id: u64,
    request: &(impl JsonRpcMessage + Serialize),
) -> Result<Value, AcpAdapterError> {
    Ok(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": request.method(),
        "params": request,
    }))
}

fn parse_params<T: DeserializeOwned>(payload: &Value, method: &str) -> Result<T, AcpAdapterError> {
    serde_json::from_value(
        payload
            .get("params")
            .cloned()
            .ok_or_else(|| AcpAdapterError::InvalidMessage(format!("{method} has no params")))?,
    )
    .map_err(Into::into)
}

fn reject_unsupported_request(
    process: &DriverProcess,
    payload: &Value,
) -> Result<(), AcpAdapterError> {
    process.send_json(&json!({
        "jsonrpc": "2.0",
        "id": payload.get("id").cloned().unwrap_or(Value::Null),
        "error": {"code": -32601, "message": "unsupported by Hyper Term"},
    }))?;
    Ok(())
}

fn is_pending_prompt_response(payload: &Value, pending: &Option<PendingPrompt>) -> bool {
    pending.as_ref().is_some_and(|pending| {
        payload.get("id") == Some(&Value::from(pending.request_id))
            && payload.get("method").is_none()
    })
}

fn validate_provider_id(provider_id: &str) -> Result<(), AcpAdapterError> {
    if provider_id.is_empty()
        || provider_id.len() > MAX_PROVIDER_ID_BYTES
        || !provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AcpAdapterError::InvalidConfig(
            "provider id must be bounded ASCII".into(),
        ));
    }
    Ok(())
}

fn external_request_id(value: Option<&Value>) -> Result<ExternalRequestId, AcpAdapterError> {
    match value {
        Some(Value::String(value)) => Ok(ExternalRequestId::String(bounded(value.clone(), 512)?)),
        Some(Value::Number(value)) if value.is_i64() => {
            Ok(ExternalRequestId::Signed(value.as_i64().unwrap()))
        }
        Some(Value::Number(value)) if value.is_u64() => {
            Ok(ExternalRequestId::Unsigned(value.as_u64().unwrap()))
        }
        _ => Err(AcpAdapterError::InvalidMessage(
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

fn bounded(value: String, maximum: usize) -> Result<String, AcpAdapterError> {
    if value.len() > maximum {
        Err(AcpAdapterError::InvalidMessage(format!(
            "text exceeds {maximum} bytes"
        )))
    } else {
        Ok(value)
    }
}

fn sha256_value(value: &Value) -> Result<String, AcpAdapterError> {
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

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AcpAdapterError> {
    mutex.lock().map_err(|_| AcpAdapterError::LockPoisoned)
}

#[derive(Debug, Error)]
pub enum AcpAdapterError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("ACP adapter filesystem setup failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("ACP adapter JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid ACP adapter configuration: {0}")]
    InvalidConfig(String),
    #[error("brokered MCP executable digest mismatch: expected {expected}, got {actual}")]
    McpExecutableDigestMismatch { expected: String, actual: String },
    #[error("ACP agent negotiated an unsupported protocol version")]
    UnsupportedProtocol,
    #[error("invalid ACP message: {0}")]
    InvalidMessage(String),
    #[error("ACP request {request_id} timed out")]
    Timeout { request_id: u64 },
    #[error("ACP request {request_id} failed: {error}")]
    Remote { request_id: u64, error: Value },
    #[error("ACP protocol failed: {0}")]
    Protocol(String),
    #[error("ACP driver exited in state {0:?}")]
    Exited(DriverState),
    #[error("ACP message inbox exceeded its bound")]
    InboxOverflow,
    #[error("ACP pending approvals exceeded their bound")]
    ApprovalOverflow,
    #[error("ACP repeated a permission request ID")]
    DuplicateApproval,
    #[error("ACP permission request is no longer pending")]
    UnknownApproval,
    #[error("the requested ACP permission decision is unavailable")]
    DecisionUnavailable,
    #[error("an ACP prompt is already running")]
    PromptAlreadyRunning,
    #[error("invalid operation authorization: {0}")]
    InvalidAuthorization(String),
    #[error("ACP adapter lock was poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use hyper_term_protocol::OperationId;
    use tempfile::TempDir;

    use super::*;

    fn fake_agent(script: &str) -> (TempDir, PathBuf) {
        let temporary = TempDir::new().unwrap();
        let executable = temporary.path().join("fake-acp");
        std::fs::write(&executable, script).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        (temporary, executable)
    }

    fn launch(executable: &Path, workspace: &Path) -> AcpAgentClient {
        AcpAgentClient::launch(AcpAgentConfig {
            executable: executable.to_owned(),
            executable_sha256: sha256_file(executable).unwrap(),
            arguments: vec![],
            environment: BTreeMap::new(),
            implementation_version: "fixture-1".into(),
            provider_id: "fixture-acp".into(),
            workspace: workspace.to_owned(),
            brokered_mcp_server: None,
        })
        .unwrap()
    }

    #[test]
    fn acp_v1_streams_message_and_completion_with_official_schema() {
        let (_temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-1\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"ACP is live.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        );
        let workspace = TempDir::new().unwrap();
        let client = launch(&executable, workspace.path());
        let initialized = client.initialize(Duration::from_secs(2)).unwrap();
        assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
        let session_id = client.start_session(Duration::from_secs(2)).unwrap();
        assert_eq!(session_id, "session-1");
        let turn_id = client.start_turn(&session_id, "say hello").unwrap();
        assert_eq!(turn_id, "acp-turn-3");
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::MessageDelta { text, .. } if text == "ACP is live."
        ));
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::TurnCompleted { status: Some(status), .. } if status == "end_turn"
        ));
        assert_eq!(client.state().unwrap(), DriverState::Ready);
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }

    #[test]
    fn acp_session_advertises_the_digest_pinned_brokered_mcp_server() {
        let (temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s' \"$line\" > \"$ACP_CAPTURE\"; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-with-mcp\"}}' ;;\n  esac\ndone\n",
        );
        let workspace = TempDir::new().unwrap();
        let capture = temporary.path().join("session-new.json");
        let mcp = temporary.path().join("hyper-term-mcp");
        std::fs::write(&mcp, "fixture MCP").unwrap();
        let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&mcp, permissions).unwrap();
        let client = AcpAgentClient::launch(AcpAgentConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            arguments: vec![],
            environment: BTreeMap::from([("ACP_CAPTURE".into(), capture.as_os_str().to_owned())]),
            implementation_version: "fixture-1".into(),
            provider_id: "fixture-acp".into(),
            workspace: workspace.path().to_owned(),
            brokered_mcp_server: Some(AcpMcpServerConfig {
                executable: mcp.clone(),
                executable_sha256: sha256_file(&mcp).unwrap(),
                arguments: vec![
                    "--agent-mode".into(),
                    "--socket".into(),
                    "/tmp/hyperd.sock".into(),
                ],
            }),
        })
        .unwrap();

        client.initialize(Duration::from_secs(2)).unwrap();
        assert_eq!(
            client.start_session(Duration::from_secs(2)).unwrap(),
            "session-with-mcp"
        );
        let request: Value = serde_json::from_slice(&std::fs::read(&capture).unwrap()).unwrap();
        let server = &request["params"]["mcpServers"][0];
        assert_eq!(server["name"], "hyper_term");
        assert_eq!(
            server["command"].as_str(),
            mcp.canonicalize().unwrap().to_str()
        );
        assert_eq!(
            server["args"],
            json!(["--agent-mode", "--socket", "/tmp/hyperd.sock"])
        );
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }

    #[test]
    fn acp_brokered_mcp_consent_is_correlated_and_forwarded_to_the_real_broker() {
        let (temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-mcp\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-mcp\",\"update\":{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"mcp-call-1\",\"kind\":\"execute\",\"title\":\"mcp.hyper_term.hyper_term.genui.compile\",\"status\":\"pending\",\"rawInput\":{\"server\":\"hyper_term\",\"tool\":\"hyper_term.genui.compile\",\"arguments\":{\"source\":\"export default function App() { return null }\"}},\"_meta\":{\"is_mcp_tool_call\":true}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"mcp-consent-1\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-mcp\",\"toolCall\":{\"toolCallId\":\"mcp-call-1\",\"kind\":\"execute\",\"status\":\"pending\"},\"_meta\":{\"is_mcp_tool_approval\":true},\"options\":[{\"optionId\":\"allow_once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"decline\",\"name\":\"Decline\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"mcp-consent-1\"'*'\"optionId\":\"allow_once\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        );
        let workspace = TempDir::new().unwrap();
        let mcp = temporary.path().join("hyper-term-mcp");
        std::fs::write(&mcp, "fixture MCP").unwrap();
        let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&mcp, permissions).unwrap();
        let client = AcpAgentClient::launch(AcpAgentConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            arguments: vec![],
            environment: BTreeMap::new(),
            implementation_version: "fixture-1".into(),
            provider_id: "fixture-acp".into(),
            workspace: workspace.path().to_owned(),
            brokered_mcp_server: Some(AcpMcpServerConfig {
                executable: mcp.clone(),
                executable_sha256: sha256_file(&mcp).unwrap(),
                arguments: vec!["--agent-mode".into()],
            }),
        })
        .unwrap();

        client.initialize(Duration::from_secs(2)).unwrap();
        let session_id = client.start_session(Duration::from_secs(2)).unwrap();
        client
            .start_turn(&session_id, "compile the counter")
            .unwrap();
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::ProtocolNotice {
                method: Some(method),
                ..
            } if method == "session/update"
        ));
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::ProtocolNotice {
                method: Some(method),
                ..
            } if method == "session/request_permission"
        ));
        assert!(client.pending_effects().unwrap().is_empty());
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::TurnCompleted { .. }
        ));
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }

    #[test]
    fn acp_unmatched_mcp_consent_remains_a_fail_closed_effect() {
        let (temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-mcp\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"unmatched-consent\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-mcp\",\"toolCall\":{\"toolCallId\":\"unseen-call\",\"kind\":\"execute\",\"status\":\"pending\"},\"_meta\":{\"is_mcp_tool_approval\":true},\"options\":[{\"optionId\":\"allow_once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"decline\",\"name\":\"Decline\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"unmatched-consent\"'*'\"optionId\":\"decline\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        );
        let workspace = TempDir::new().unwrap();
        let mcp = temporary.path().join("hyper-term-mcp");
        std::fs::write(&mcp, "fixture MCP").unwrap();
        let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&mcp, permissions).unwrap();
        let client = AcpAgentClient::launch(AcpAgentConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            arguments: vec![],
            environment: BTreeMap::new(),
            implementation_version: "fixture-1".into(),
            provider_id: "fixture-acp".into(),
            workspace: workspace.path().to_owned(),
            brokered_mcp_server: Some(AcpMcpServerConfig {
                executable: mcp.clone(),
                executable_sha256: sha256_file(&mcp).unwrap(),
                arguments: vec!["--agent-mode".into()],
            }),
        })
        .unwrap();

        client.initialize(Duration::from_secs(2)).unwrap();
        let session_id = client.start_session(Duration::from_secs(2)).unwrap();
        client
            .start_turn(&session_id, "attempt an unmatched call")
            .unwrap();
        let proposal = match client.next_event(Duration::from_secs(2)).unwrap() {
            AgentDriverEvent::EffectProposed { proposal, .. } => proposal,
            event => panic!("unexpected event: {event:?}"),
        };
        assert_eq!(proposal.kind, AgentEffectKind::Shell);
        client
            .resolve_effect(
                &proposal.request_id,
                AgentEffectAuthorization {
                    operation_id: OperationId::new(),
                    operation_revision: 1,
                    proposal_sha256: proposal.payload_sha256,
                    decision: PermissionDecision::RejectOnce,
                },
            )
            .unwrap();
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::TurnCompleted { .. }
        ));
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }

    #[test]
    fn acp_permission_becomes_brokered_proposal_and_rejection() {
        let (_temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-2\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"permission-7\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-2\",\"toolCall\":{\"toolCallId\":\"tool-1\",\"kind\":\"execute\",\"title\":\"Run cargo test\"},\"options\":[{\"optionId\":\"allow-once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"reject-once\",\"name\":\"Reject\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"permission-7\"'*'\"optionId\":\"reject-once\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        );
        let workspace = TempDir::new().unwrap();
        let client = launch(&executable, workspace.path());
        client.initialize(Duration::from_secs(2)).unwrap();
        let session_id = client.start_session(Duration::from_secs(2)).unwrap();
        client.start_turn(&session_id, "test it").unwrap();
        let proposal = match client.next_event(Duration::from_secs(2)).unwrap() {
            AgentDriverEvent::EffectProposed { proposal, .. } => proposal,
            event => panic!("unexpected event: {event:?}"),
        };
        assert_eq!(proposal.protocol, StructuredAgentProtocol::Acp);
        assert_eq!(proposal.kind, AgentEffectKind::Shell);
        assert_eq!(proposal.summary, "Run cargo test");
        client
            .resolve_effect(
                &proposal.request_id,
                AgentEffectAuthorization {
                    operation_id: OperationId::new(),
                    operation_revision: 1,
                    proposal_sha256: proposal.payload_sha256,
                    decision: PermissionDecision::RejectOnce,
                },
            )
            .unwrap();
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::TurnCompleted { .. }
        ));
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }
}
