use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agent_client_protocol::JsonRpcMessage;
use agent_client_protocol::schema::{ProtocolVersion, v1};
use hyper_term_protocol::{
    AgentMediaKind, AgentPlanEntry, AgentPlanPriority, AgentPlanStatus, AgentToolCall,
    AgentToolContent, AgentToolKind, AgentToolLocation, AgentToolStatus, PermissionDecision,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use thiserror::Error;
use uuid::Uuid;

use crate::codex_containment::{apply_managed_proxy_environment, compile_agent_task_sandbox};
use crate::{
    AgentAvailableCommand, AgentClientError, AgentContainmentConfig, AgentDriverEvent,
    AgentEffectAuthorization, AgentEffectKind, AgentEffectProposal, AgentSessionCapabilities,
    AgentSessionConfigChoice, AgentSessionConfigKind, AgentSessionConfigOption,
    AgentSessionConfigValue, DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES, DriverError, DriverEvent,
    DriverFraming, DriverKind, DriverManifest, DriverProcess, DriverSpec, DriverState,
    ExternalRequestId, StructuredAgentClient, StructuredAgentProtocol, process::BoundedDriverInbox,
    sha256_file,
};

const ACP_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUFFERED_MESSAGES: usize = 512;
const MAX_BUFFERED_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_APPROVALS: usize = 128;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_SESSION_CONFIG_OPTIONS: usize = 24;
const MAX_SESSION_CONFIG_CHOICES: usize = 96;
const MAX_AVAILABLE_COMMANDS: usize = 96;
const MAX_CAPABILITY_ID_BYTES: usize = 128;
const MAX_CAPABILITY_LABEL_BYTES: usize = 256;
const MAX_CAPABILITY_DESCRIPTION_BYTES: usize = 2048;
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

    fn session_capabilities(&self) -> Result<AgentSessionCapabilities, AgentClientError> {
        Ok(AcpAgentClient::session_capabilities(self)?)
    }

    fn set_session_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: AgentSessionConfigValue,
        timeout: Duration,
    ) -> Result<AgentSessionCapabilities, AgentClientError> {
        Ok(AcpAgentClient::set_session_config_option(
            self, session_id, config_id, value, timeout,
        )?)
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
    pub containment: Option<AgentContainmentConfig>,
}

pub struct AcpAgentClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<BoundedDriverInbox>,
    pending_prompt: Mutex<Option<PendingPrompt>>,
    pending_approvals: Mutex<HashMap<ExternalRequestId, PendingApproval>>,
    brokered_mcp_calls: Mutex<HashMap<String, BrokeredMcpCall>>,
    tool_calls: Mutex<HashMap<String, v1::ToolCall>>,
    session_capabilities: Mutex<AgentSessionCapabilities>,
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
        let driver_id = Uuid::new_v4();
        let authority_environment = config.environment.clone();
        let mut environment = config.environment;
        let sandbox = match config.containment.as_ref() {
            Some(containment) => {
                apply_managed_proxy_environment(
                    &mut environment,
                    &containment.credentialed_proxy_url,
                );
                let mut read_paths = containment.read_paths.clone();
                if let Some(mcp) = &config.brokered_mcp_server {
                    read_paths.push(mcp.executable.clone());
                }
                Some(compile_agent_task_sandbox(
                    driver_id,
                    &config.executable,
                    &config.arguments,
                    &workspace,
                    &environment,
                    &authority_environment,
                    &containment.proxy_url,
                    &containment.allowed_hosts,
                    &containment.allowed_unix_sockets,
                    read_paths,
                    containment.write_paths.clone(),
                )?)
            }
            None => None,
        };
        let permission_profile = sandbox
            .as_ref()
            .map(crate::sandbox_permission_profile)
            .unwrap_or_else(|| "acp-proposal-only-v1".into());
        let process = DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id,
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
                permission_profile,
            },
            executable: config.executable,
            arguments: config.arguments,
            working_directory: workspace.clone(),
            environment,
            sandbox,
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
            tool_calls: Mutex::new(HashMap::new()),
            session_capabilities: Mutex::new(AgentSessionCapabilities::default()),
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
        let config_options = normalize_config_options(response.config_options.unwrap_or_default())?;
        lock(&self.session_capabilities)?.config_options = config_options;
        bounded(response.session_id.to_string(), 4096)
    }

    pub fn session_capabilities(&self) -> Result<AgentSessionCapabilities, AcpAdapterError> {
        Ok(lock(&self.session_capabilities)?.clone())
    }

    pub fn set_session_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: AgentSessionConfigValue,
        timeout: Duration,
    ) -> Result<AgentSessionCapabilities, AcpAdapterError> {
        let session_id = bounded(session_id.to_owned(), 4096)?;
        let config_id = bounded(config_id.to_owned(), MAX_CAPABILITY_ID_BYTES)?;
        let normalized_value = match value {
            AgentSessionConfigValue::Id { value } => AgentSessionConfigValue::Id {
                value: bounded(value, MAX_CAPABILITY_ID_BYTES)?,
            },
            AgentSessionConfigValue::Boolean { value } => {
                AgentSessionConfigValue::Boolean { value }
            }
        };
        {
            let capabilities = lock(&self.session_capabilities)?;
            validate_config_value(&capabilities, &config_id, &normalized_value)?;
        }
        let value = match normalized_value {
            AgentSessionConfigValue::Id { value } => v1::SessionConfigOptionValue::value_id(value),
            AgentSessionConfigValue::Boolean { value } => {
                v1::SessionConfigOptionValue::boolean(value)
            }
        };
        let request = v1::SetSessionConfigOptionRequest::new(session_id, config_id, value);
        let response: v1::SetSessionConfigOptionResponse = self.request_idle(&request, timeout)?;
        let config_options = normalize_config_options(response.config_options)?;
        let mut capabilities = lock(&self.session_capabilities)?;
        capabilities.config_options = config_options;
        Ok(capabilities.clone())
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
        lock(&self.tool_calls)?.clear();
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
        self.request_locked(request, timeout)
    }

    fn request_idle<Request>(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Request::Response, AcpAdapterError>
    where
        Request: agent_client_protocol::JsonRpcRequest + Serialize,
        Request::Response: DeserializeOwned,
    {
        let _gate = lock(&self.request_gate)?;
        if lock(&self.pending_prompt)?.is_some() {
            return Err(AcpAdapterError::PromptAlreadyRunning);
        }
        self.request_locked(request, timeout)
    }

    fn request_locked<Request>(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Request::Response, AcpAdapterError>
    where
        Request: agent_client_protocol::JsonRpcRequest + Serialize,
        Request::Response: DeserializeOwned,
    {
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
            v1::SessionUpdate::Plan(plan) => Ok(AgentDriverEvent::PlanUpdated {
                sequence,
                thread_id,
                turn_id,
                entries: plan
                    .entries
                    .into_iter()
                    .map(normalize_plan_entry)
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            v1::SessionUpdate::ToolCall(call) => {
                let id = bounded(call.tool_call_id.to_string(), 4096)?;
                let normalized = normalize_tool_call(&call)?;
                lock(&self.tool_calls)?.insert(id, call);
                Ok(AgentDriverEvent::ToolCallUpdated {
                    sequence,
                    thread_id,
                    turn_id,
                    call: normalized,
                })
            }
            v1::SessionUpdate::ToolCallUpdate(update) => {
                let id = bounded(update.tool_call_id.to_string(), 4096)?;
                let call = {
                    let mut calls = lock(&self.tool_calls)?;
                    if let Some(call) = calls.get_mut(&id) {
                        call.update(update.fields);
                        call.clone()
                    } else {
                        let call = v1::ToolCall::try_from(update).map_err(|error| {
                            AcpAdapterError::InvalidMessage(format!(
                                "ACP tool update cannot create {id}: {error}"
                            ))
                        })?;
                        calls.insert(id, call.clone());
                        call
                    }
                };
                Ok(AgentDriverEvent::ToolCallUpdated {
                    sequence,
                    thread_id,
                    turn_id,
                    call: normalize_tool_call(&call)?,
                })
            }
            v1::SessionUpdate::AvailableCommandsUpdate(update) => {
                lock(&self.session_capabilities)?.available_commands =
                    normalize_available_commands(update.available_commands)?;
                protocol_notice(
                    sequence,
                    Some("session/update"),
                    &serde_json::to_value("available_commands_update")?,
                )
            }
            v1::SessionUpdate::ConfigOptionUpdate(update) => {
                lock(&self.session_capabilities)?.config_options =
                    normalize_config_options(update.config_options)?;
                protocol_notice(
                    sequence,
                    Some("session/update"),
                    &serde_json::to_value("config_option_update")?,
                )
            }
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

fn normalize_plan_entry(entry: v1::PlanEntry) -> Result<AgentPlanEntry, AcpAdapterError> {
    Ok(AgentPlanEntry {
        content: bounded(entry.content, 16 * 1024)?,
        priority: match entry.priority {
            v1::PlanEntryPriority::High => AgentPlanPriority::High,
            v1::PlanEntryPriority::Medium => AgentPlanPriority::Medium,
            v1::PlanEntryPriority::Low => AgentPlanPriority::Low,
            _ => AgentPlanPriority::Medium,
        },
        status: match entry.status {
            v1::PlanEntryStatus::Pending => AgentPlanStatus::Pending,
            v1::PlanEntryStatus::InProgress => AgentPlanStatus::InProgress,
            v1::PlanEntryStatus::Completed => AgentPlanStatus::Completed,
            _ => AgentPlanStatus::Pending,
        },
    })
}

fn normalize_tool_call(call: &v1::ToolCall) -> Result<AgentToolCall, AcpAdapterError> {
    Ok(AgentToolCall {
        tool_call_id: bounded(call.tool_call_id.to_string(), 4096)?,
        title: bounded(call.title.clone(), 16 * 1024)?,
        kind: match call.kind {
            v1::ToolKind::Read => AgentToolKind::Read,
            v1::ToolKind::Edit => AgentToolKind::Edit,
            v1::ToolKind::Delete => AgentToolKind::Delete,
            v1::ToolKind::Move => AgentToolKind::Move,
            v1::ToolKind::Search => AgentToolKind::Search,
            v1::ToolKind::Execute => AgentToolKind::Execute,
            v1::ToolKind::Think => AgentToolKind::Think,
            v1::ToolKind::Fetch => AgentToolKind::Fetch,
            v1::ToolKind::SwitchMode => AgentToolKind::SwitchMode,
            _ => AgentToolKind::Other,
        },
        status: match call.status {
            v1::ToolCallStatus::Pending => AgentToolStatus::Pending,
            v1::ToolCallStatus::InProgress => AgentToolStatus::InProgress,
            v1::ToolCallStatus::Completed => AgentToolStatus::Completed,
            v1::ToolCallStatus::Failed => AgentToolStatus::Failed,
            _ => AgentToolStatus::Pending,
        },
        content: call
            .content
            .iter()
            .map(normalize_tool_content)
            .collect::<Result<Vec<_>, _>>()?,
        locations: call
            .locations
            .iter()
            .map(|location| {
                Ok(AgentToolLocation {
                    path: bounded(location.path.to_string_lossy().into_owned(), 16 * 1024)?,
                    line: location.line,
                })
            })
            .collect::<Result<Vec<_>, AcpAdapterError>>()?,
        raw_input: normalize_raw_value(call.raw_input.as_ref())?,
        raw_output: normalize_raw_value(call.raw_output.as_ref())?,
    })
}

fn normalize_raw_value(value: Option<&Value>) -> Result<Option<String>, AcpAdapterError> {
    value
        .map(|value| bounded(serde_json::to_string(value)?, 32 * 1024))
        .transpose()
}

fn normalize_tool_content(
    content: &v1::ToolCallContent,
) -> Result<AgentToolContent, AcpAdapterError> {
    match content {
        v1::ToolCallContent::Content(content) => normalize_content_block(&content.content),
        v1::ToolCallContent::Diff(diff) => {
            let old = diff.old_text.as_deref().unwrap_or("");
            let text_diff = TextDiff::from_lines(old, &diff.new_text);
            let added_lines = text_diff
                .iter_all_changes()
                .filter(|change| change.tag() == ChangeTag::Insert)
                .count()
                .try_into()
                .unwrap_or(u32::MAX);
            let removed_lines = text_diff
                .iter_all_changes()
                .filter(|change| change.tag() == ChangeTag::Delete)
                .count()
                .try_into()
                .unwrap_or(u32::MAX);
            let label = diff.path.to_string_lossy();
            let patch = text_diff
                .unified_diff()
                .context_radius(3)
                .header(&label, &label)
                .to_string();
            Ok(AgentToolContent::Diff {
                path: bounded(label.into_owned(), 16 * 1024)?,
                patch: bounded(patch, 64 * 1024)?,
                added_lines,
                removed_lines,
            })
        }
        v1::ToolCallContent::Terminal(terminal) => Ok(AgentToolContent::Terminal {
            terminal_id: bounded(terminal.terminal_id.to_string(), 4096)?,
        }),
        _ => Ok(AgentToolContent::Text {
            text: "Unsupported ACP tool content".into(),
        }),
    }
}

fn normalize_content_block(
    content: &v1::ContentBlock,
) -> Result<AgentToolContent, AcpAdapterError> {
    match content {
        v1::ContentBlock::Text(text) => Ok(AgentToolContent::Text {
            text: bounded(text.text.clone(), 64 * 1024)?,
        }),
        v1::ContentBlock::Image(image) => Ok(AgentToolContent::Media {
            kind: AgentMediaKind::Image,
            mime_type: bounded(image.mime_type.clone(), 512)?,
            uri: image
                .uri
                .clone()
                .map(|uri| bounded(uri, 16 * 1024))
                .transpose()?,
            encoded_bytes: image.data.len().try_into().unwrap_or(u64::MAX),
        }),
        v1::ContentBlock::Audio(audio) => Ok(AgentToolContent::Media {
            kind: AgentMediaKind::Audio,
            mime_type: bounded(audio.mime_type.clone(), 512)?,
            uri: None,
            encoded_bytes: audio.data.len().try_into().unwrap_or(u64::MAX),
        }),
        v1::ContentBlock::ResourceLink(link) => Ok(AgentToolContent::Resource {
            name: bounded(
                link.title.clone().unwrap_or_else(|| link.name.clone()),
                4096,
            )?,
            uri: bounded(link.uri.clone(), 16 * 1024)?,
            mime_type: link
                .mime_type
                .clone()
                .map(|mime| bounded(mime, 512))
                .transpose()?,
            text: link
                .description
                .clone()
                .map(|text| bounded(text, 16 * 1024))
                .transpose()?,
            byte_count: link.size.and_then(|size| size.try_into().ok()),
        }),
        v1::ContentBlock::Resource(resource) => match &resource.resource {
            v1::EmbeddedResourceResource::TextResourceContents(value) => {
                Ok(AgentToolContent::Resource {
                    name: bounded(value.uri.clone(), 4096)?,
                    uri: bounded(value.uri.clone(), 16 * 1024)?,
                    mime_type: value
                        .mime_type
                        .clone()
                        .map(|mime| bounded(mime, 512))
                        .transpose()?,
                    text: Some(bounded(value.text.clone(), 64 * 1024)?),
                    byte_count: None,
                })
            }
            v1::EmbeddedResourceResource::BlobResourceContents(value) => {
                Ok(AgentToolContent::Resource {
                    name: bounded(value.uri.clone(), 4096)?,
                    uri: bounded(value.uri.clone(), 16 * 1024)?,
                    mime_type: value
                        .mime_type
                        .clone()
                        .map(|mime| bounded(mime, 512))
                        .transpose()?,
                    text: None,
                    byte_count: Some(value.blob.len().try_into().unwrap_or(u64::MAX)),
                })
            }
            _ => Ok(AgentToolContent::Text {
                text: "Unsupported ACP resource content".into(),
            }),
        },
        _ => Ok(AgentToolContent::Text {
            text: "Unsupported ACP content block".into(),
        }),
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

fn normalize_config_options(
    options: Vec<v1::SessionConfigOption>,
) -> Result<Vec<AgentSessionConfigOption>, AcpAdapterError> {
    if options.len() > MAX_SESSION_CONFIG_OPTIONS {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP session configuration exceeded its option bound".into(),
        ));
    }
    options.into_iter().map(normalize_config_option).collect()
}

fn normalize_config_option(
    option: v1::SessionConfigOption,
) -> Result<AgentSessionConfigOption, AcpAdapterError> {
    let id = bounded(option.id.to_string(), MAX_CAPABILITY_ID_BYTES)?;
    let name = bounded(option.name, MAX_CAPABILITY_LABEL_BYTES)?;
    let description = option
        .description
        .map(|value| bounded(value, MAX_CAPABILITY_DESCRIPTION_BYTES))
        .transpose()?;
    let category = option.category.and_then(|category| match category {
        v1::SessionConfigOptionCategory::Mode => Some("mode".to_owned()),
        v1::SessionConfigOptionCategory::Model => Some("model".to_owned()),
        v1::SessionConfigOptionCategory::ModelConfig => Some("model_config".to_owned()),
        v1::SessionConfigOptionCategory::ThoughtLevel => Some("thought_level".to_owned()),
        v1::SessionConfigOptionCategory::Other(value) => {
            bounded(value, MAX_CAPABILITY_ID_BYTES).ok()
        }
        _ => None,
    });
    let (kind, choices) = match option.kind {
        v1::SessionConfigKind::Select(select) => {
            let current_value = bounded(select.current_value.to_string(), MAX_CAPABILITY_ID_BYTES)?;
            let choices = normalize_config_choices(select.options)?;
            if !choices.iter().any(|choice| choice.value == current_value) {
                return Err(AcpAdapterError::InvalidMessage(format!(
                    "ACP session configuration {id} selected an unavailable value"
                )));
            }
            (AgentSessionConfigKind::Select { current_value }, choices)
        }
        v1::SessionConfigKind::Boolean(boolean) => (
            AgentSessionConfigKind::Boolean {
                current_value: boolean.current_value,
            },
            Vec::new(),
        ),
        _ => {
            return Err(AcpAdapterError::InvalidMessage(format!(
                "ACP session configuration {id} has an unsupported kind"
            )));
        }
    };
    Ok(AgentSessionConfigOption {
        id,
        name,
        description,
        category,
        kind,
        choices,
    })
}

fn normalize_config_choices(
    options: v1::SessionConfigSelectOptions,
) -> Result<Vec<AgentSessionConfigChoice>, AcpAdapterError> {
    let mut choices = Vec::new();
    match options {
        v1::SessionConfigSelectOptions::Ungrouped(options) => {
            for option in options {
                choices.push(normalize_config_choice(option, None)?);
            }
        }
        v1::SessionConfigSelectOptions::Grouped(groups) => {
            for group in groups {
                let group_name = bounded(group.name, MAX_CAPABILITY_LABEL_BYTES)?;
                for option in group.options {
                    choices.push(normalize_config_choice(option, Some(group_name.clone()))?);
                }
            }
        }
        _ => {
            return Err(AcpAdapterError::InvalidMessage(
                "ACP session configuration uses unsupported choice grouping".into(),
            ));
        }
    }
    if choices.len() > MAX_SESSION_CONFIG_CHOICES {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP session configuration exceeded its choice bound".into(),
        ));
    }
    Ok(choices)
}

fn normalize_config_choice(
    option: v1::SessionConfigSelectOption,
    group: Option<String>,
) -> Result<AgentSessionConfigChoice, AcpAdapterError> {
    Ok(AgentSessionConfigChoice {
        value: bounded(option.value.to_string(), MAX_CAPABILITY_ID_BYTES)?,
        name: bounded(option.name, MAX_CAPABILITY_LABEL_BYTES)?,
        description: option
            .description
            .map(|value| bounded(value, MAX_CAPABILITY_DESCRIPTION_BYTES))
            .transpose()?,
        group,
    })
}

fn normalize_available_commands(
    commands: Vec<v1::AvailableCommand>,
) -> Result<Vec<AgentAvailableCommand>, AcpAdapterError> {
    if commands.len() > MAX_AVAILABLE_COMMANDS {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP available commands exceeded their bound".into(),
        ));
    }
    commands
        .into_iter()
        .map(|command| {
            let input_hint = match command.input {
                Some(v1::AvailableCommandInput::Unstructured(input)) => {
                    Some(bounded(input.hint, MAX_CAPABILITY_DESCRIPTION_BYTES)?)
                }
                _ => None,
            };
            Ok(AgentAvailableCommand {
                name: bounded(command.name, MAX_CAPABILITY_ID_BYTES)?,
                description: bounded(command.description, MAX_CAPABILITY_DESCRIPTION_BYTES)?,
                input_hint,
            })
        })
        .collect()
}

fn validate_config_value(
    capabilities: &AgentSessionCapabilities,
    config_id: &str,
    value: &AgentSessionConfigValue,
) -> Result<(), AcpAdapterError> {
    let option = capabilities
        .config_options
        .iter()
        .find(|option| option.id == config_id)
        .ok_or_else(|| {
            AcpAdapterError::InvalidMessage(format!(
                "ACP session configuration {config_id} is unavailable"
            ))
        })?;
    match (&option.kind, value) {
        (AgentSessionConfigKind::Select { .. }, AgentSessionConfigValue::Id { value })
            if option.choices.iter().any(|choice| choice.value == *value) =>
        {
            Ok(())
        }
        (AgentSessionConfigKind::Boolean { .. }, AgentSessionConfigValue::Boolean { .. }) => Ok(()),
        _ => Err(AcpAdapterError::InvalidMessage(format!(
            "ACP session configuration {config_id} rejected the requested value"
        ))),
    }
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
            containment: None,
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
        let initialized = client.initialize(Duration::from_secs(10)).unwrap();
        assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
        let session_id = client.start_session(Duration::from_secs(10)).unwrap();
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
    fn acp_session_capabilities_are_bounded_replaced_and_configurable() {
        let (temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-config\",\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"gpt-a\",\"options\":[{\"value\":\"gpt-a\",\"name\":\"GPT A\"},{\"value\":\"gpt-b\",\"name\":\"GPT B\"}]}]}}' ;;\n    *'\"method\":\"session/set_config_option\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"gpt-b\",\"options\":[{\"value\":\"gpt-a\",\"name\":\"GPT A\"},{\"value\":\"gpt-b\",\"name\":\"GPT B\"}]}]}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-config\",\"update\":{\"sessionUpdate\":\"available_commands_update\",\"availableCommands\":[{\"name\":\"skills\",\"description\":\"Configure skills\"}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-config\",\"update\":{\"sessionUpdate\":\"config_option_update\",\"configOptions\":[{\"id\":\"thought\",\"name\":\"Reasoning\",\"category\":\"thought_level\",\"type\":\"select\",\"currentValue\":\"high\",\"options\":[{\"value\":\"high\",\"name\":\"High\"}]}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        );
        let client = launch(&executable, temporary.path());
        client.initialize(Duration::from_secs(10)).unwrap();
        let session_id = client.start_session(Duration::from_secs(10)).unwrap();

        let initial = client.session_capabilities().unwrap();
        assert_eq!(initial.config_options[0].id, "model");
        assert_eq!(initial.config_options[0].choices.len(), 2);
        assert!(
            client
                .set_session_config_option(
                    &session_id,
                    "model",
                    AgentSessionConfigValue::Id {
                        value: "missing".into(),
                    },
                    Duration::from_secs(10),
                )
                .is_err()
        );
        let updated = client
            .set_session_config_option(
                &session_id,
                "model",
                AgentSessionConfigValue::Id {
                    value: "gpt-b".into(),
                },
                Duration::from_secs(10),
            )
            .unwrap();
        assert!(matches!(
            &updated.config_options[0].kind,
            AgentSessionConfigKind::Select { current_value } if current_value == "gpt-b"
        ));

        client.start_turn(&session_id, "show capabilities").unwrap();
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::ProtocolNotice { .. }
        ));
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::ProtocolNotice { .. }
        ));
        let capabilities = client.session_capabilities().unwrap();
        assert_eq!(capabilities.available_commands[0].name, "skills");
        assert_eq!(capabilities.config_options[0].id, "thought");
    }

    #[test]
    fn acp_v1_preserves_plan_tool_diff_terminal_resource_and_updates() {
        let (_temporary, executable) = fake_agent(
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-structured\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-structured\",\"update\":{\"sessionUpdate\":\"plan\",\"entries\":[{\"content\":\"Inspect the workspace\",\"priority\":\"high\",\"status\":\"in_progress\"}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-structured\",\"update\":{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"edit-1\",\"title\":\"Edit src/lib.rs\",\"kind\":\"edit\",\"status\":\"in_progress\",\"locations\":[{\"path\":\"/tmp/src/lib.rs\",\"line\":7}],\"content\":[{\"type\":\"diff\",\"path\":\"/tmp/src/lib.rs\",\"oldText\":\"old\\n\",\"newText\":\"new\\n\"},{\"type\":\"terminal\",\"terminalId\":\"terminal-7\"},{\"type\":\"content\",\"content\":{\"type\":\"text\",\"text\":\"Applied edit\"}},{\"type\":\"content\",\"content\":{\"type\":\"resource_link\",\"name\":\"build log\",\"uri\":\"file:///tmp/build.log\",\"mimeType\":\"text/plain\"}}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-structured\",\"update\":{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"edit-1\",\"status\":\"completed\",\"rawOutput\":{\"ok\":true}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        );
        let workspace = TempDir::new().unwrap();
        let client = launch(&executable, workspace.path());
        client.initialize(Duration::from_secs(10)).unwrap();
        let session_id = client.start_session(Duration::from_secs(10)).unwrap();
        client.start_turn(&session_id, "make the edit").unwrap();

        let plan = client.next_event(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            plan,
            AgentDriverEvent::PlanUpdated { entries, .. }
                if entries.len() == 1
                    && entries[0].content == "Inspect the workspace"
                    && entries[0].status == AgentPlanStatus::InProgress
        ));
        let tool = client.next_event(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            tool,
            AgentDriverEvent::ToolCallUpdated { call, .. }
                if call.status == AgentToolStatus::InProgress
                    && call.locations.len() == 1
                    && matches!(&call.content[0], AgentToolContent::Diff { added_lines: 1, removed_lines: 1, patch, .. } if patch.contains("-old") && patch.contains("+new"))
                    && matches!(&call.content[1], AgentToolContent::Terminal { terminal_id } if terminal_id == "terminal-7")
                    && matches!(&call.content[2], AgentToolContent::Text { text } if text == "Applied edit")
                    && matches!(&call.content[3], AgentToolContent::Resource { uri, .. } if uri == "file:///tmp/build.log")
        ));
        let completed = client.next_event(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            completed,
            AgentDriverEvent::ToolCallUpdated { call, .. }
                if call.status == AgentToolStatus::Completed
                    && call.content.len() == 4
                    && call.raw_output.as_deref() == Some("{\"ok\":true}")
        ));
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::TurnCompleted { .. }
        ));
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
            containment: None,
        })
        .unwrap();

        client.initialize(Duration::from_secs(10)).unwrap();
        assert_eq!(
            client.start_session(Duration::from_secs(10)).unwrap(),
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
            containment: None,
        })
        .unwrap();

        client.initialize(Duration::from_secs(10)).unwrap();
        let session_id = client.start_session(Duration::from_secs(10)).unwrap();
        client
            .start_turn(&session_id, "compile the counter")
            .unwrap();
        assert!(matches!(
            client.next_event(Duration::from_secs(2)).unwrap(),
            AgentDriverEvent::ToolCallUpdated { call, .. }
                if call.title == "mcp.hyper_term.hyper_term.genui.compile"
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
            containment: None,
        })
        .unwrap();

        client.initialize(Duration::from_secs(10)).unwrap();
        let session_id = client.start_session(Duration::from_secs(10)).unwrap();
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
        client.initialize(Duration::from_secs(10)).unwrap();
        let session_id = client.start_session(Duration::from_secs(10)).unwrap();
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

    #[cfg(target_os = "macos")]
    #[test]
    fn contained_acp_can_handshake_but_cannot_read_host_or_write_workspace() {
        use std::net::TcpListener;

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let scratch = root.path().join("scratch");
        let secret = root.path().join("host-secret.txt");
        let marker = scratch.join("boundary.txt");
        let forbidden = workspace.join("provider-write.txt");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(&secret, "must stay outside the ACP sandbox").unwrap();
        let script = format!(
            "#!/bin/sh\nif /bin/cat {secret} >/dev/null 2>&1; then host=allowed; else host=denied; fi\nif /usr/bin/touch {forbidden} >/dev/null 2>&1; then workspace=allowed; else workspace=denied; fi\nprintf '%s,%s\\n' \"$host\" \"$workspace\" > {marker}\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":1,\"agentCapabilities\":{{}},\"authMethods\":[]}}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"sessionId\":\"contained-session\"}}}}' ;;\n  esac\ndone\n",
            secret = secret.display(),
            forbidden = forbidden.display(),
            marker = marker.display(),
        );
        let executable = root.path().join("contained-acp");
        std::fs::write(&executable, script).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let credentialed_proxy_url =
            proxy_url.replacen("http://", "http://hyper-term:contained-test-token@", 1);
        let environment = BTreeMap::from([
            ("HOME".into(), scratch.clone().into_os_string()),
            ("PATH".into(), OsString::from("/usr/bin:/bin")),
            ("TERM".into(), OsString::from("dumb")),
            ("TMPDIR".into(), scratch.clone().into_os_string()),
        ]);
        let client = AcpAgentClient::launch(AcpAgentConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            arguments: Vec::new(),
            environment,
            implementation_version: "contained-fixture-1".into(),
            provider_id: "contained-fixture-acp".into(),
            workspace: workspace.canonicalize().unwrap(),
            brokered_mcp_server: None,
            containment: Some(AgentContainmentConfig {
                proxy_url,
                credentialed_proxy_url,
                allowed_hosts: vec!["api.example.com".into()],
                allowed_unix_sockets: Vec::new(),
                read_paths: Vec::new(),
                write_paths: vec![scratch.canonicalize().unwrap()],
            }),
        })
        .unwrap();

        client.initialize(Duration::from_secs(10)).unwrap();
        assert_eq!(
            client.start_session(Duration::from_secs(10)).unwrap(),
            "contained-session"
        );
        assert_eq!(
            std::fs::read_to_string(marker).unwrap().trim(),
            "denied,denied"
        );
        assert!(!forbidden.exists());
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }
}
