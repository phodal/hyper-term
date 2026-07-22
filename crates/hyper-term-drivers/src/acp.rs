use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agent_client_protocol::JsonRpcMessage;
use agent_client_protocol::schema::{ProtocolVersion, v1};
#[cfg(test)]
use hyper_term_protocol::AgentPlanStatus;
use hyper_term_protocol::{
    AgentMediaKind, AgentToolCall, AgentToolContent, AgentToolKind, AgentToolLocation,
    AgentToolStatus, ContextReceipt, PermissionDecision,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use thiserror::Error;
use uuid::Uuid;

use crate::acp_capabilities::{
    ACP_SESSION_MODE_CONFIG_ID, MAX_CAPABILITY_ID_BYTES, apply_session_capability_update,
    normalize_available_commands, normalize_session_capabilities,
    replace_config_options_preserving_mode, update_session_mode, validate_config_value,
};
use crate::acp_session_update::normalize_content_update;
use crate::codex_containment::{
    agent_task_sandbox_profile, apply_managed_proxy_environment,
    compile_agent_task_sandbox_from_profile,
};
use crate::execution_context::{
    compile_agent_execution_context, compile_mcp_execution_context, os_environment,
};
use crate::{
    AgentClientError, AgentContainmentConfig, AgentDriverEvent, AgentEffectAuthorization,
    AgentEffectKind, AgentEffectProposal, AgentHostOperation, AgentHostRequest, AgentHostResponse,
    AgentSessionCapabilities, AgentSessionConfigValue, AgentTerminalEnvironmentVariable,
    DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES, DriverError, DriverEvent, DriverFraming, DriverKind,
    DriverManifest, DriverProcess, DriverSpec, DriverState, ExternalRequestId,
    StructuredAgentClient, StructuredAgentProtocol, process::BoundedDriverInbox, sha256_file,
};

const ACP_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUFFERED_MESSAGES: usize = 512;
const MAX_BUFFERED_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_APPROVALS: usize = 128;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_PENDING_HOST_REQUESTS: usize = 128;
const MAX_TERMINAL_ARGUMENTS: usize = 256;
const MAX_TERMINAL_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_TERMINAL_ENVIRONMENT: usize = 64;
const MAX_TERMINAL_ENVIRONMENT_BYTES: usize = 64 * 1024;
const DEFAULT_TERMINAL_OUTPUT_BYTES: u64 = 512 * 1024;
const MAX_TERMINAL_OUTPUT_BYTES: u64 = 2 * 1024 * 1024;
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
    pub runtime_home: PathBuf,
    pub runtime_temp: PathBuf,
}

impl StructuredAgentClient for AcpAgentClient {
    fn provider_id(&self) -> &str {
        AcpAgentClient::provider_id(self)
    }

    fn protocol(&self) -> StructuredAgentProtocol {
        StructuredAgentProtocol::Acp
    }

    fn execution_context_receipts(&self) -> Vec<ContextReceipt> {
        [
            self.context_receipt.clone(),
            self.mcp_context_receipt.clone(),
        ]
        .into_iter()
        .flatten()
        .collect()
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

    fn cancel_turn(&self, session_id: &str, _turn_id: &str) -> Result<(), AgentClientError> {
        Ok(AcpAgentClient::cancel_turn(self, session_id)?)
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

    fn resolve_host_request(
        &self,
        request_id: &ExternalRequestId,
        response: AgentHostResponse,
    ) -> Result<(), AgentClientError> {
        Ok(AcpAgentClient::resolve_host_request(
            self, request_id, response,
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
    pub terminal_client: bool,
}

pub struct AcpAgentClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<BoundedDriverInbox>,
    pending_prompt: Mutex<Option<PendingPrompt>>,
    pending_approvals: Mutex<HashMap<ExternalRequestId, PendingApproval>>,
    pending_host_requests: Mutex<HashMap<ExternalRequestId, AgentHostRequest>>,
    brokered_mcp_calls: Mutex<HashMap<String, BrokeredMcpCall>>,
    tool_calls: Mutex<HashMap<String, v1::ToolCall>>,
    session_capabilities: Mutex<AgentSessionCapabilities>,
    provider_id: String,
    workspace: PathBuf,
    mcp_servers: Vec<v1::McpServer>,
    brokered_mcp: bool,
    terminal_client: bool,
    context_receipt: Option<ContextReceipt>,
    mcp_context_receipt: Option<ContextReceipt>,
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
        let driver_id = Uuid::new_v4();
        let brokered_mcp = config.brokered_mcp_server.is_some();
        let copilot_launch_mcp = config.provider_id == "copilot-acp" && brokered_mcp;
        let mut arguments = config.arguments;
        if copilot_launch_mcp {
            let mcp = config.brokered_mcp_server.as_ref().unwrap();
            let mcp_environment = copilot_mcp_environment(mcp, &config.environment)?;
            arguments.extend(copilot_mcp_arguments(mcp, &mcp_environment)?);
        }
        let mut environment = config.environment;
        let mut context_receipt = None;
        let mut mcp_context_receipt = None;
        let mut mcp_environment = BTreeMap::new();
        let sandbox = match config.containment.as_ref() {
            Some(containment) => {
                let mut read_paths = containment.read_paths.clone();
                if let Some(mcp) = &config.brokered_mcp_server {
                    read_paths.push(mcp.executable.clone());
                }
                let profile = agent_task_sandbox_profile(
                    &config.executable,
                    &workspace,
                    &environment,
                    &containment.proxy_url,
                    &containment.allowed_hosts,
                    &containment.allowed_unix_sockets,
                    read_paths,
                    containment.write_paths.clone(),
                )?;
                let (context, receipt) = compile_agent_execution_context(
                    driver_id,
                    &config.provider_id,
                    &workspace,
                    &environment,
                    profile,
                    &containment.proxy_url,
                )?;
                let profile = context.requested_sandbox.clone().ok_or_else(|| {
                    AcpAdapterError::InvalidConfig(
                        "ACP execution context did not compile a sandbox".into(),
                    )
                })?;
                if let Some(mcp) = &config.brokered_mcp_server {
                    let (mcp_context, receipt) = compile_mcp_execution_context(
                        driver_id,
                        &workspace,
                        mcp.runtime_home.clone(),
                        mcp.runtime_temp.clone(),
                        &context,
                        profile.clone(),
                    )?;
                    mcp_environment = mcp_context.environment.variables;
                    mcp_context_receipt = Some(receipt);
                }
                environment = os_environment(&context);
                apply_managed_proxy_environment(
                    &mut environment,
                    &containment.credentialed_proxy_url,
                );
                let sandbox = compile_agent_task_sandbox_from_profile(
                    driver_id,
                    &config.executable,
                    &arguments,
                    &workspace,
                    &environment,
                    profile,
                )?;
                context_receipt = Some(receipt);
                Some(sandbox)
            }
            None => None,
        };
        let mcp_servers = if copilot_launch_mcp {
            Vec::new()
        } else {
            config
                .brokered_mcp_server
                .as_ref()
                .map(|config| acp_mcp_server(config, &mcp_environment))
                .transpose()?
                .into_iter()
                .collect()
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
            arguments,
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
            pending_host_requests: Mutex::new(HashMap::new()),
            brokered_mcp_calls: Mutex::new(HashMap::new()),
            tool_calls: Mutex::new(HashMap::new()),
            session_capabilities: Mutex::new(AgentSessionCapabilities::default()),
            provider_id: config.provider_id,
            workspace,
            mcp_servers,
            brokered_mcp,
            terminal_client: config.terminal_client,
            context_receipt,
            mcp_context_receipt,
        })
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn context_receipt(&self) -> Option<&ContextReceipt> {
        self.context_receipt.as_ref()
    }

    pub fn mcp_context_receipt(&self) -> Option<&ContextReceipt> {
        self.mcp_context_receipt.as_ref()
    }

    pub fn initialize(&self, timeout: Duration) -> Result<v1::InitializeResponse, AcpAdapterError> {
        let request = v1::InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(v1::ClientCapabilities::new().terminal(self.terminal_client))
            .client_info(
                v1::Implementation::new("hyper-term", env!("CARGO_PKG_VERSION"))
                    .title("Hyper Term"),
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
        let config_options = normalize_session_capabilities(
            response.modes,
            response.config_options.unwrap_or_default(),
        )?;
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
        if config_id == ACP_SESSION_MODE_CONFIG_ID {
            let AgentSessionConfigValue::Id { value } = normalized_value else {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP session mode requires an ID value".into(),
                ));
            };
            let request = v1::SetSessionModeRequest::new(session_id, value.clone());
            let _: v1::SetSessionModeResponse = self.request_idle(&request, timeout)?;
            let mut capabilities = lock(&self.session_capabilities)?;
            update_session_mode(&mut capabilities, value)?;
            return Ok(capabilities.clone());
        }
        let value = match normalized_value {
            AgentSessionConfigValue::Id { value } => v1::SessionConfigOptionValue::value_id(value),
            AgentSessionConfigValue::Boolean { value } => {
                v1::SessionConfigOptionValue::boolean(value)
            }
        };
        let request = v1::SetSessionConfigOptionRequest::new(session_id, config_id, value);
        let response: v1::SetSessionConfigOptionResponse = self.request_idle(&request, timeout)?;
        let mut capabilities = lock(&self.session_capabilities)?;
        replace_config_options_preserving_mode(&mut capabilities, response.config_options)?;
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

    pub fn cancel_turn(&self, session_id: &str) -> Result<(), AcpAdapterError> {
        let session_id = bounded(session_id.to_owned(), 4096)?;
        let _gate = lock(&self.request_gate)?;
        let pending = lock(&self.pending_prompt)?
            .clone()
            .ok_or(AcpAdapterError::NoActivePrompt)?;
        if pending.session_id != session_id {
            return Err(AcpAdapterError::NoActivePrompt);
        }

        // ACP requires every outstanding permission request to be answered as
        // cancelled when the client cancels active session work.
        let approval_ids = lock(&self.pending_approvals)?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in &approval_ids {
            let response =
                v1::RequestPermissionResponse::new(v1::RequestPermissionOutcome::Cancelled);
            self.process.send_json(&json!({
                "jsonrpc": "2.0",
                "id": request_id_value(request_id),
                "result": response,
            }))?;
        }
        lock(&self.pending_approvals)?.clear();

        // Host requests are also bounded provider-owned work. Return an
        // explicit cancellation error so they cannot remain retained forever.
        let host_request_ids = lock(&self.pending_host_requests)?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in &host_request_ids {
            self.process.send_json(&json!({
                "jsonrpc": "2.0",
                "id": request_id_value(request_id),
                "error": {"code": -32800, "message": "cancelled by Hyper Term"},
            }))?;
        }
        lock(&self.pending_host_requests)?.clear();

        let notification = v1::CancelNotification::new(session_id);
        self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "method": notification.method(),
            "params": notification,
        }))?;
        Ok(())
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

    pub fn resolve_host_request(
        &self,
        request_id: &ExternalRequestId,
        response: AgentHostResponse,
    ) -> Result<(), AcpAdapterError> {
        let request = lock(&self.pending_host_requests)?
            .get(request_id)
            .cloned()
            .ok_or(AcpAdapterError::UnknownHostRequest)?;
        let wire = host_response_value(&request.operation, response)?;
        let message = match wire {
            HostResponseValue::Result(result) => json!({
                "jsonrpc": "2.0",
                "id": request_id_value(request_id),
                "result": result,
            }),
            HostResponseValue::Error { code, message } => json!({
                "jsonrpc": "2.0",
                "id": request_id_value(request_id),
                "error": {"code": code, "message": message},
            }),
        };
        self.process.send_json(&message)?;
        lock(&self.pending_host_requests)?.remove(request_id);
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
            Some(
                "terminal/create"
                | "terminal/output"
                | "terminal/release"
                | "terminal/wait_for_exit"
                | "terminal/kill",
            ) if self.terminal_client => self.normalize_host_request(sequence, payload),
            Some(_) if payload.get("id").is_some() => {
                reject_unsupported_request(&self.process, &payload)?;
                protocol_notice(sequence, method, &payload)
            }
            _ => protocol_notice(sequence, method, &payload),
        }
    }

    fn normalize_host_request(
        &self,
        sequence: u64,
        payload: Value,
    ) -> Result<AgentDriverEvent, AcpAdapterError> {
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AcpAdapterError::InvalidMessage("ACP host request has no method".into())
            })?;
        let request_id = external_request_id(payload.get("id"))?;
        if lock(&self.pending_approvals)?.contains_key(&request_id) {
            return Err(AcpAdapterError::DuplicateApproval);
        }
        let params = payload.get("params").cloned().unwrap_or(Value::Null);
        let pending_prompt = lock(&self.pending_prompt)?.clone().ok_or_else(|| {
            AcpAdapterError::InvalidMessage("ACP host request arrived outside a turn".into())
        })?;
        let operation =
            normalize_host_operation(method, &params, &pending_prompt.session_id, &self.workspace)?;
        let request = AgentHostRequest {
            driver_id: self.process.manifest().driver_id,
            protocol: StructuredAgentProtocol::Acp,
            request_id: request_id.clone(),
            method: method.to_owned(),
            payload_sha256: sha256_value(&params)?,
            thread_id: pending_prompt.session_id,
            turn_id: pending_prompt.turn_id,
            operation,
        };
        let mut pending = lock(&self.pending_host_requests)?;
        if pending.len() == MAX_PENDING_HOST_REQUESTS {
            return Err(AcpAdapterError::HostRequestOverflow);
        }
        if pending.insert(request_id, request.clone()).is_some() {
            return Err(AcpAdapterError::DuplicateHostRequest);
        }
        Ok(AgentDriverEvent::HostRequest { sequence, request })
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
        let mut capabilities = lock(&self.session_capabilities)?;
        let capability_update =
            apply_session_capability_update(&mut capabilities, &notification.update)?;
        drop(capabilities);
        if let Some(kind) = capability_update {
            return protocol_notice(
                sequence,
                Some("session/update"),
                &serde_json::to_value(kind)?,
            );
        }
        if let Some(event) =
            normalize_content_update(sequence, &thread_id, &turn_id, &notification.update)?
        {
            return Ok(event);
        }
        match notification.update {
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
                let normalized = normalize_available_commands(update.available_commands);
                let notice_method = if normalized.truncated {
                    "session/update/available_commands_truncated"
                } else {
                    "session/update"
                };
                let retained = normalized.commands.len();
                lock(&self.session_capabilities)?.available_commands = normalized.commands;
                protocol_notice(
                    sequence,
                    Some(notice_method),
                    &json!({
                        "type": "available_commands_update",
                        "retained": retained,
                        "truncated": normalized.truncated,
                    }),
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
        if lock(&self.pending_host_requests)?.contains_key(&request_id) {
            return Err(AcpAdapterError::DuplicateHostRequest);
        }
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
                let Some(tool_name) = brokered_mcp_tool(&self.provider_id, call) else {
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
        // Codex and Claude ACP ask their client for transport-level consent
        // before they send the correlated tools/call to the configured MCP
        // server. Forward that consent only for a previously observed,
        // allowlisted Hyper Term tool.
        // The digest-pinned MCP server still validates the exact arguments and
        // creates the user-visible Rust broker operation before executing it.
        if !self.brokered_mcp {
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

fn acp_mcp_server(
    config: &AcpMcpServerConfig,
    environment: &BTreeMap<String, String>,
) -> Result<v1::McpServer, AcpAdapterError> {
    let (executable, arguments, environment) = validated_mcp_stdio(config, environment.clone())?;
    let environment = environment
        .iter()
        .map(|(name, value)| v1::EnvVariable::new(name, value))
        .collect();
    Ok(v1::McpServer::Stdio(
        v1::McpServerStdio::new("hyper_term", executable)
            .args(arguments)
            .env(environment),
    ))
}

fn copilot_mcp_environment(
    config: &AcpMcpServerConfig,
    provider_environment: &BTreeMap<String, OsString>,
) -> Result<BTreeMap<String, String>, AcpAdapterError> {
    let mut environment = BTreeMap::new();
    for (name, path) in [
        ("HOME", config.runtime_home.as_path()),
        ("TMPDIR", config.runtime_temp.as_path()),
    ] {
        let value = path.to_str().ok_or_else(|| {
            AcpAdapterError::InvalidConfig(format!("brokered MCP {name} path is not UTF-8"))
        })?;
        environment.insert(name.into(), value.into());
    }
    for (name, default) in [
        ("PATH", "/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
        ("TZ", "UTC"),
        ("TERM", "dumb"),
    ] {
        let value = provider_environment
            .get(name)
            .map(|value| {
                value.to_str().ok_or_else(|| {
                    AcpAdapterError::InvalidConfig(format!(
                        "brokered MCP {name} environment is not UTF-8"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(default);
        environment.insert(name.into(), value.into());
    }
    Ok(environment)
}

fn copilot_mcp_arguments(
    config: &AcpMcpServerConfig,
    environment: &BTreeMap<String, String>,
) -> Result<Vec<OsString>, AcpAdapterError> {
    let (executable, arguments, environment) = validated_mcp_stdio(config, environment.clone())?;
    let launch_config = serde_json::to_string(&json!({
        "mcpServers": {
            "hyper_term": {
                "type": "local",
                "command": executable,
                "args": arguments,
                "env": environment,
                "tools": ["*"],
            }
        }
    }))?;
    Ok(vec!["--additional-mcp-config".into(), launch_config.into()])
}

type ValidatedMcpStdio = (PathBuf, Vec<String>, BTreeMap<String, String>);

fn validated_mcp_stdio(
    config: &AcpMcpServerConfig,
    environment: BTreeMap<String, String>,
) -> Result<ValidatedMcpStdio, AcpAdapterError> {
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
    Ok((executable, arguments, environment))
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

enum HostResponseValue {
    Result(Value),
    Error { code: i32, message: String },
}

fn normalize_host_operation(
    method: &str,
    params: &Value,
    pending_session_id: &str,
    workspace: &Path,
) -> Result<AgentHostOperation, AcpAdapterError> {
    match method {
        "terminal/create" => {
            let request: v1::CreateTerminalRequest = serde_json::from_value(params.clone())?;
            validate_host_session(&request.session_id.to_string(), pending_session_id)?;
            if request.args.len() > MAX_TERMINAL_ARGUMENTS {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal argument count exceeded its bound".into(),
                ));
            }
            let command = bounded(request.command, 16 * 1024)?;
            if command.is_empty() {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal command is empty".into(),
                ));
            }
            let mut argument_bytes = 0usize;
            let args = request
                .args
                .into_iter()
                .map(|argument| {
                    let argument = bounded(argument, 16 * 1024)?;
                    argument_bytes =
                        argument_bytes.checked_add(argument.len()).ok_or_else(|| {
                            AcpAdapterError::InvalidMessage(
                                "ACP terminal argument bytes overflowed".into(),
                            )
                        })?;
                    Ok(argument)
                })
                .collect::<Result<Vec<_>, AcpAdapterError>>()?;
            if argument_bytes > MAX_TERMINAL_ARGUMENT_BYTES {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal arguments exceeded their byte bound".into(),
                ));
            }
            if request.env.len() > MAX_TERMINAL_ENVIRONMENT {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal environment exceeded its entry bound".into(),
                ));
            }
            let mut names = HashSet::new();
            let mut environment_bytes = 0usize;
            let env = request
                .env
                .into_iter()
                .map(|variable| {
                    let name = bounded(variable.name, 128)?;
                    if !valid_environment_name(&name) || !names.insert(name.clone()) {
                        return Err(AcpAdapterError::InvalidMessage(
                            "ACP terminal environment contains an invalid or duplicate name".into(),
                        ));
                    }
                    let value = bounded(variable.value, 16 * 1024)?;
                    environment_bytes = environment_bytes
                        .checked_add(name.len())
                        .and_then(|bytes| bytes.checked_add(value.len()))
                        .ok_or_else(|| {
                            AcpAdapterError::InvalidMessage(
                                "ACP terminal environment bytes overflowed".into(),
                            )
                        })?;
                    Ok(AgentTerminalEnvironmentVariable { name, value })
                })
                .collect::<Result<Vec<_>, AcpAdapterError>>()?;
            if environment_bytes > MAX_TERMINAL_ENVIRONMENT_BYTES {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal environment exceeded its byte bound".into(),
                ));
            }
            let cwd = request.cwd.unwrap_or_else(|| workspace.to_path_buf());
            if !cwd.is_absolute() {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal cwd must be absolute".into(),
                ));
            }
            let cwd = cwd.canonicalize().map_err(|_| {
                AcpAdapterError::InvalidMessage("ACP terminal cwd is unavailable".into())
            })?;
            if !cwd.starts_with(workspace) {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal cwd escaped the session workspace".into(),
                ));
            }
            let output_byte_limit = request
                .output_byte_limit
                .unwrap_or(DEFAULT_TERMINAL_OUTPUT_BYTES);
            if output_byte_limit == 0 || output_byte_limit > MAX_TERMINAL_OUTPUT_BYTES {
                return Err(AcpAdapterError::InvalidMessage(
                    "ACP terminal output limit is outside the supported bound".into(),
                ));
            }
            Ok(AgentHostOperation::TerminalCreate {
                command,
                args,
                env,
                cwd,
                output_byte_limit,
            })
        }
        "terminal/output" => {
            let request: v1::TerminalOutputRequest = serde_json::from_value(params.clone())?;
            validate_host_session(&request.session_id.to_string(), pending_session_id)?;
            Ok(AgentHostOperation::TerminalOutput {
                terminal_id: normalize_terminal_id(request.terminal_id.to_string())?,
            })
        }
        "terminal/release" => {
            let request: v1::ReleaseTerminalRequest = serde_json::from_value(params.clone())?;
            validate_host_session(&request.session_id.to_string(), pending_session_id)?;
            Ok(AgentHostOperation::TerminalRelease {
                terminal_id: normalize_terminal_id(request.terminal_id.to_string())?,
            })
        }
        "terminal/wait_for_exit" => {
            let request: v1::WaitForTerminalExitRequest = serde_json::from_value(params.clone())?;
            validate_host_session(&request.session_id.to_string(), pending_session_id)?;
            Ok(AgentHostOperation::TerminalWaitForExit {
                terminal_id: normalize_terminal_id(request.terminal_id.to_string())?,
            })
        }
        "terminal/kill" => {
            let request: v1::KillTerminalRequest = serde_json::from_value(params.clone())?;
            validate_host_session(&request.session_id.to_string(), pending_session_id)?;
            Ok(AgentHostOperation::TerminalKill {
                terminal_id: normalize_terminal_id(request.terminal_id.to_string())?,
            })
        }
        _ => Err(AcpAdapterError::InvalidMessage(
            "unsupported ACP host request".into(),
        )),
    }
}

fn validate_host_session(actual: &str, expected: &str) -> Result<(), AcpAdapterError> {
    if actual != expected {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP host request belongs to another session".into(),
        ));
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn normalize_terminal_id(terminal_id: String) -> Result<String, AcpAdapterError> {
    let terminal_id = bounded(terminal_id, 4096)?;
    if terminal_id.is_empty() {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP terminal id is empty".into(),
        ));
    }
    Ok(terminal_id)
}

fn host_response_value(
    operation: &AgentHostOperation,
    response: AgentHostResponse,
) -> Result<HostResponseValue, AcpAdapterError> {
    if let AgentHostResponse::Error { code, message } = response {
        return Ok(HostResponseValue::Error {
            code,
            message: bounded(message, 512)?,
        });
    }
    let result = match (operation, response) {
        (
            AgentHostOperation::TerminalCreate { .. },
            AgentHostResponse::TerminalCreated { terminal_id },
        ) => json!({"terminalId": normalize_terminal_id(terminal_id)?}),
        (
            AgentHostOperation::TerminalOutput { .. },
            AgentHostResponse::TerminalOutput {
                output,
                truncated,
                exit_code,
                signal,
            },
        ) => {
            let output = bounded(output, MAX_TERMINAL_OUTPUT_BYTES as usize)?;
            let signal = signal.map(|signal| bounded(signal, 128)).transpose()?;
            json!({
                "output": output,
                "truncated": truncated,
                "exitStatus": {"exitCode": exit_code, "signal": signal},
            })
        }
        (AgentHostOperation::TerminalRelease { .. }, AgentHostResponse::TerminalReleased) => {
            json!({})
        }
        (
            AgentHostOperation::TerminalWaitForExit { .. },
            AgentHostResponse::TerminalExited { exit_code, signal },
        ) => json!({
            "exitCode": exit_code,
            "signal": signal.map(|signal| bounded(signal, 128)).transpose()?,
        }),
        (AgentHostOperation::TerminalKill { .. }, AgentHostResponse::TerminalKilled) => json!({}),
        _ => return Err(AcpAdapterError::HostResponseMismatch),
    };
    Ok(HostResponseValue::Result(result))
}

fn brokered_mcp_tool<'a>(provider_id: &str, call: &'a v1::ToolCall) -> Option<&'a str> {
    let codex_shape = call.kind == v1::ToolKind::Execute
        && call
            .meta
            .as_ref()
            .and_then(|meta| meta.get("is_mcp_tool_call"))
            .and_then(Value::as_bool)
            == Some(true);
    if codex_shape {
        let input = call.raw_input.as_ref()?.as_object()?;
        if input.get("server").and_then(Value::as_str) != Some("hyper_term")
            || !input.get("arguments").is_some_and(Value::is_object)
        {
            return None;
        }
        let tool_name = input.get("tool").and_then(Value::as_str)?;
        return (HYPER_TERM_MCP_TOOLS.contains(&tool_name)
            && call.title == format!("mcp.hyper_term.{tool_name}"))
        .then_some(tool_name);
    }

    call.raw_input.as_ref()?.as_object()?;
    match provider_id {
        // Claude ACP exposes MCP calls with a flattened title.
        "claude-acp" => HYPER_TERM_MCP_TOOLS.iter().copied().find(|tool_name| {
            call.title == format!("mcp__hyper_term__{}", tool_name.replace(['.', '-'], "_"))
        }),
        // Copilot ACP uses the configured server name followed by a second,
        // hyphen-flattened tool name. Match only names derived from the
        // explicit allowlist; never trust an arbitrary title transformation.
        "copilot-acp" => HYPER_TERM_MCP_TOOLS.iter().copied().find(|tool_name| {
            call.title == format!("hyper_term-{}", tool_name.replace(['.', '-'], "-"))
        }),
        _ => None,
    }
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

pub(super) fn bounded(value: String, maximum: usize) -> Result<String, AcpAdapterError> {
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
    #[error("ACP pending Agent-to-Host requests exceeded their bound")]
    HostRequestOverflow,
    #[error("ACP repeated a permission request ID")]
    DuplicateApproval,
    #[error("ACP permission request is no longer pending")]
    UnknownApproval,
    #[error("ACP repeated an Agent-to-Host request ID")]
    DuplicateHostRequest,
    #[error("ACP Agent-to-Host request is no longer pending")]
    UnknownHostRequest,
    #[error("ACP Agent-to-Host response did not match its request")]
    HostResponseMismatch,
    #[error("the requested ACP permission decision is unavailable")]
    DecisionUnavailable,
    #[error("an ACP prompt is already running")]
    PromptAlreadyRunning,
    #[error("ACP session has no active prompt to cancel")]
    NoActivePrompt,
    #[error("invalid operation authorization: {0}")]
    InvalidAuthorization(String),
    #[error("ACP adapter lock was poisoned")]
    LockPoisoned,
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;
