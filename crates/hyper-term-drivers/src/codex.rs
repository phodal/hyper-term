use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hyper_term_protocol::{AgentPlanEntry, AgentPlanPriority, AgentPlanStatus, PermissionDecision};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::codex_containment::{
    AgentContainmentConfig, apply_managed_proxy_environment, compile_agent_task_sandbox,
};
use crate::{
    AgentAvailableCommand, AgentClientError, AgentDriverEvent, AgentEffectAuthorization,
    AgentEffectKind, AgentEffectProposal, AgentGoalStatus, AgentSessionCapabilities,
    AgentSessionConfigChoice, AgentSessionConfigKind, AgentSessionConfigOption,
    AgentSessionConfigValue, AgentThreadGoal, DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES, DriverError,
    DriverEvent, DriverFraming, DriverKind, DriverManifest, DriverProcess, DriverSpec, DriverState,
    ExternalRequestId, StructuredAgentClient, StructuredAgentProtocol, process::BoundedDriverInbox,
    sha256_file,
};

const MAX_PENDING_APPROVALS: usize = 128;
const MAX_BUFFERED_MESSAGES: usize = 512;
const MAX_BUFFERED_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const CODEX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_CODEX_MODELS: usize = 24;
const MAX_CODEX_REASONING_EFFORTS: usize = 16;
// Reserve one slot in the desktop's bounded command palette for `/goal`.
const MAX_CODEX_SKILLS: usize = 23;
const MAX_CODEX_PLAN_ENTRIES: usize = 128;
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
    pub containment: Option<AgentContainmentConfig>,
}

pub struct CodexAppServerClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<BoundedDriverInbox>,
    pending: Mutex<HashMap<ExternalRequestId, AgentEffectProposal>>,
    model_catalog: Mutex<Vec<CodexModelCapability>>,
    session_capabilities: Mutex<AgentSessionCapabilities>,
    turn_config: Mutex<CodexTurnConfig>,
    thread_goal: Mutex<Option<AgentThreadGoal>>,
    workspace: String,
    staged_auth_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
struct CodexTurnConfig {
    model: Option<String>,
    reasoning_effort: Option<String>,
}

#[derive(Clone, Debug)]
struct CodexModelCapability {
    model: String,
    display_name: String,
    description: String,
    default_reasoning_effort: String,
    reasoning_efforts: Vec<CodexReasoningEffortWire>,
    is_default: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexModelWire {
    model: String,
    display_name: String,
    description: String,
    hidden: bool,
    supported_reasoning_efforts: Vec<CodexReasoningEffortWire>,
    default_reasoning_effort: String,
    is_default: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexReasoningEffortWire {
    reasoning_effort: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct CodexModelListWire {
    data: Vec<CodexModelWire>,
}

#[derive(Debug, Deserialize)]
struct CodexSkillsListWire {
    data: Vec<CodexSkillGroupWire>,
}

#[derive(Debug, Deserialize)]
struct CodexSkillGroupWire {
    skills: Vec<CodexSkillWire>,
}

#[derive(Debug, Deserialize)]
struct CodexSkillWire {
    name: String,
    description: String,
    enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexThreadGoalWire {
    objective: String,
    status: String,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
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
        let auth_read_path = config.auth_file.clone();
        let staged_auth_file = stage_codex_auth_file(config.auth_file.as_deref(), &codex_home)?;
        let mut environment = BTreeMap::from([
            ("CODEX_HOME".into(), codex_home.into_os_string()),
            ("HOME".into(), scratch.clone().into_os_string()),
            ("NO_COLOR".into(), OsString::from("1")),
            (
                "PATH".into(),
                OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"),
            ),
            ("RUST_BACKTRACE".into(), OsString::from("0")),
            ("TERM".into(), OsString::from("dumb")),
            ("TMPDIR".into(), scratch.into_os_string()),
        ]);
        let authority_environment = environment.clone();
        if let Some(containment) = &config.containment {
            apply_managed_proxy_environment(&mut environment, &containment.credentialed_proxy_url);
        }
        let driver_id = Uuid::new_v4();
        let arguments = codex_arguments(config.brokered_mcp_server.as_ref())?;
        let sandbox = match config.containment.as_ref() {
            Some(containment) => {
                let mut read_paths = containment.read_paths.clone();
                if let Some(auth_file) = auth_read_path {
                    read_paths.push(auth_file);
                }
                if let Some(mcp) = &config.brokered_mcp_server {
                    read_paths.push(mcp.executable.clone());
                }
                match compile_agent_task_sandbox(
                    driver_id,
                    &config.executable,
                    &arguments,
                    &workspace,
                    &environment,
                    &authority_environment,
                    &containment.proxy_url,
                    &containment.allowed_hosts,
                    &containment.allowed_unix_sockets,
                    read_paths,
                    containment.write_paths.clone(),
                ) {
                    Ok(plan) => Some(plan),
                    Err(error) => {
                        remove_staged_auth_file(staged_auth_file.as_ref());
                        return Err(error.into());
                    }
                }
            }
            None => None,
        };
        let permission_profile = sandbox
            .as_ref()
            .map(crate::sandbox_permission_profile)
            .unwrap_or_else(|| "codex-proposal-only-v1".into());
        let process = match DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id,
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
                permission_profile,
            },
            executable: config.executable,
            arguments,
            working_directory: workspace,
            environment,
            sandbox,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: CODEX_FRAME_BYTES,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
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
            inbox: Mutex::new(BoundedDriverInbox::new(
                MAX_BUFFERED_MESSAGES,
                MAX_BUFFERED_MESSAGE_BYTES,
            )),
            pending: Mutex::new(HashMap::new()),
            model_catalog: Mutex::new(Vec::new()),
            session_capabilities: Mutex::new(AgentSessionCapabilities::default()),
            turn_config: Mutex::new(CodexTurnConfig::default()),
            thread_goal: Mutex::new(None),
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
                "ephemeral": false
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
        let turn_config = lock(&self.turn_config)?.clone();
        self.process.begin_effect()?;
        let mut params = json!({
            "threadId": thread_id,
            "input": [{
                "type": "text",
                "text": prompt,
                "text_elements": []
            }]
        });
        if let Some(model) = turn_config.model {
            params["model"] = Value::String(model);
        }
        if let Some(reasoning_effort) = turn_config.reasoning_effort {
            params["effort"] = Value::String(reasoning_effort);
        }
        let response = self.request_raw("turn/start", params, timeout);
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

    pub fn cancel_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), CodexAdapterError> {
        let thread_id = bounded(thread_id.to_owned(), 4096)?;
        let turn_id = bounded(turn_id.to_owned(), 4096)?;
        if thread_id.is_empty() || turn_id.is_empty() {
            return Err(CodexAdapterError::InvalidMessage(
                "turn/interrupt requires thread and turn ids".into(),
            ));
        }
        let _gate = lock(&self.request_gate)?;
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        // The event pump remains the sole reader while a turn is active. The
        // app-server response is consumed as a protocol notice; turn/completed
        // with status=interrupted is the authoritative acknowledgement.
        self.process.send_json(&json!({
            "id": id,
            "method": "turn/interrupt",
            "params": {"threadId": thread_id, "turnId": turn_id}
        }))?;
        Ok(())
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

    fn refresh_session_capabilities(
        &self,
        timeout: Duration,
    ) -> Result<AgentSessionCapabilities, CodexAdapterError> {
        let models_response = self.request_raw(
            "model/list",
            json!({"cursor": null, "limit": MAX_CODEX_MODELS, "includeHidden": false}),
            timeout,
        )?;
        let models: CodexModelListWire =
            serde_json::from_value(models_response.get("result").cloned().ok_or_else(|| {
                CodexAdapterError::InvalidMessage("model/list returned no result".into())
            })?)?;
        let catalog = normalize_codex_models(models.data)?;
        let selected = catalog
            .iter()
            .find(|model| model.is_default)
            .or_else(|| catalog.first())
            .ok_or_else(|| {
                CodexAdapterError::InvalidMessage("model/list returned no usable models".into())
            })?;
        let turn_config = CodexTurnConfig {
            model: Some(selected.model.clone()),
            reasoning_effort: Some(selected.default_reasoning_effort.clone()),
        };
        let mut available_commands = self
            .request_raw(
                "skills/list",
                json!({"cwds": [self.workspace], "forceReload": false}),
                timeout,
            )
            .and_then(normalize_codex_skills)
            .unwrap_or_default();
        available_commands.push(AgentAvailableCommand {
            name: "goal".into(),
            description: "Set or clear a persistent Codex goal".into(),
            input_hint: Some("Describe the goal, or type clear".into()),
        });
        let capabilities = AgentSessionCapabilities {
            config_options: codex_config_options(&catalog, &turn_config)?,
            available_commands,
        };
        *lock(&self.model_catalog)? = catalog;
        *lock(&self.turn_config)? = turn_config;
        *lock(&self.session_capabilities)? = capabilities.clone();
        Ok(capabilities)
    }

    fn update_session_config(
        &self,
        config_id: &str,
        value: AgentSessionConfigValue,
    ) -> Result<AgentSessionCapabilities, CodexAdapterError> {
        let value = match value {
            AgentSessionConfigValue::Id { value } => bounded(value, 128)?,
            AgentSessionConfigValue::Boolean { .. } => {
                return Err(CodexAdapterError::InvalidConfig(
                    "Codex model settings require an id value".into(),
                ));
            }
        };
        let catalog = lock(&self.model_catalog)?;
        let mut turn_config = lock(&self.turn_config)?;
        match config_id {
            "model" => {
                let selected = catalog
                    .iter()
                    .find(|model| model.model == value)
                    .ok_or_else(|| {
                        CodexAdapterError::InvalidConfig("unknown Codex model".into())
                    })?;
                turn_config.model = Some(selected.model.clone());
                turn_config.reasoning_effort = Some(selected.default_reasoning_effort.clone());
            }
            "reasoning_effort" => {
                let selected = selected_codex_model(&catalog, &turn_config)?;
                if !selected
                    .reasoning_efforts
                    .iter()
                    .any(|effort| effort.reasoning_effort == value)
                {
                    return Err(CodexAdapterError::InvalidConfig(
                        "unsupported Codex reasoning effort".into(),
                    ));
                }
                turn_config.reasoning_effort = Some(value);
            }
            _ => {
                return Err(CodexAdapterError::InvalidConfig(
                    "unknown Codex session configuration".into(),
                ));
            }
        }
        let mut capabilities = lock(&self.session_capabilities)?;
        capabilities.config_options = codex_config_options(&catalog, &turn_config)?;
        Ok(capabilities.clone())
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
                event => lock(&self.inbox)?
                    .push_back(event)
                    .map_err(|_| CodexAdapterError::InboxOverflow)?,
            }
        }
    }

    fn set_goal(
        &self,
        thread_id: &str,
        objective: Option<&str>,
        status: Option<AgentGoalStatus>,
        timeout: Duration,
    ) -> Result<AgentThreadGoal, CodexAdapterError> {
        let thread_id = bounded(thread_id.to_owned(), 4096)?;
        let objective = objective
            .map(|value| bounded(value.trim().to_owned(), 16 * 1024))
            .transpose()?;
        let status = status.map(codex_goal_status_name);
        let response = self.request_raw(
            "thread/goal/set",
            json!({"threadId": thread_id, "objective": objective, "status": status}),
            timeout,
        )?;
        let wire: CodexThreadGoalWire =
            serde_json::from_value(response.pointer("/result/goal").cloned().ok_or_else(
                || CodexAdapterError::InvalidMessage("thread/goal/set returned no goal".into()),
            )?)?;
        let goal = normalize_codex_goal(wire)?;
        *lock(&self.thread_goal)? = Some(goal.clone());
        Ok(goal)
    }

    fn clear_goal(&self, thread_id: &str, timeout: Duration) -> Result<bool, CodexAdapterError> {
        let thread_id = bounded(thread_id.to_owned(), 4096)?;
        let response =
            self.request_raw("thread/goal/clear", json!({"threadId": thread_id}), timeout)?;
        let cleared = response
            .pointer("/result/cleared")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                CodexAdapterError::InvalidMessage("thread/goal/clear returned no result".into())
            })?;
        if cleared {
            *lock(&self.thread_goal)? = None;
        }
        Ok(cleared)
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
            Some("thread/goal/updated") => {
                let wire: CodexThreadGoalWire =
                    serde_json::from_value(params.get("goal").cloned().ok_or_else(|| {
                        CodexAdapterError::InvalidMessage(
                            "thread goal update omitted its goal".into(),
                        )
                    })?)?;
                *lock(&self.thread_goal)? = Some(normalize_codex_goal(wire)?);
                Ok(AgentDriverEvent::ProtocolNotice {
                    sequence,
                    method: method.map(ToOwned::to_owned),
                    payload_sha256: sha256_value(&payload)?,
                })
            }
            Some("thread/goal/cleared") => {
                *lock(&self.thread_goal)? = None;
                Ok(AgentDriverEvent::ProtocolNotice {
                    sequence,
                    method: method.map(ToOwned::to_owned),
                    payload_sha256: sha256_value(&payload)?,
                })
            }
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
            Some("turn/plan/updated") => Ok(AgentDriverEvent::PlanUpdated {
                sequence,
                thread_id: required_string(params, "threadId")?,
                turn_id: required_string(params, "turnId")?,
                entries: normalize_codex_plan(params)?,
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

impl StructuredAgentClient for CodexAppServerClient {
    fn provider_id(&self) -> &str {
        "codex"
    }

    fn protocol(&self) -> StructuredAgentProtocol {
        StructuredAgentProtocol::CodexAppServerV2
    }

    fn initialize_session(&self, timeout: Duration) -> Result<String, AgentClientError> {
        CodexAppServerClient::initialize(self, timeout)?;
        // Capability discovery is additive: older app-server builds can still
        // run a thread even when they do not expose the richer Codex catalogs.
        let _ = self.refresh_session_capabilities(timeout);
        Ok(CodexAppServerClient::start_thread(self, timeout)?)
    }

    fn start_turn(
        &self,
        session_id: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, AgentClientError> {
        Ok(CodexAppServerClient::start_turn(
            self, session_id, prompt, timeout,
        )?)
    }

    fn next_event(&self, timeout: Duration) -> Result<AgentDriverEvent, AgentClientError> {
        Ok(CodexAppServerClient::next_event(self, timeout)?)
    }

    fn cancel_turn(&self, session_id: &str, turn_id: &str) -> Result<(), AgentClientError> {
        Ok(CodexAppServerClient::cancel_turn(
            self, session_id, turn_id,
        )?)
    }

    fn session_capabilities(&self) -> Result<AgentSessionCapabilities, AgentClientError> {
        Ok(lock(&self.session_capabilities)?.clone())
    }

    fn thread_goal(&self) -> Result<Option<AgentThreadGoal>, AgentClientError> {
        Ok(lock(&self.thread_goal)?.clone())
    }

    fn set_thread_goal(
        &self,
        session_id: &str,
        objective: Option<&str>,
        status: Option<AgentGoalStatus>,
        timeout: Duration,
    ) -> Result<AgentThreadGoal, AgentClientError> {
        Ok(self.set_goal(session_id, objective, status, timeout)?)
    }

    fn clear_thread_goal(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<bool, AgentClientError> {
        Ok(self.clear_goal(session_id, timeout)?)
    }

    fn set_session_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: AgentSessionConfigValue,
        _timeout: Duration,
    ) -> Result<AgentSessionCapabilities, AgentClientError> {
        if session_id.is_empty() {
            return Err(CodexAdapterError::InvalidConfig(
                "Codex thread id must not be empty".into(),
            )
            .into());
        }
        Ok(self.update_session_config(config_id, value)?)
    }

    fn resolve_effect(
        &self,
        request_id: &ExternalRequestId,
        authorization: AgentEffectAuthorization,
    ) -> Result<(), AgentClientError> {
        Ok(CodexAppServerClient::resolve_effect(
            self,
            request_id,
            authorization,
        )?)
    }

    fn state(&self) -> Result<DriverState, AgentClientError> {
        Ok(CodexAppServerClient::state(self)?)
    }

    fn stderr_tail(&self) -> Result<String, AgentClientError> {
        Ok(CodexAppServerClient::stderr_tail(self)?)
    }

    fn close(&self) -> Result<DriverState, AgentClientError> {
        Ok(CodexAppServerClient::close(self)?)
    }
}

#[cfg(unix)]
pub fn stage_codex_auth_file(
    source: Option<&std::path::Path>,
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
pub fn stage_codex_auth_file(
    source: Option<&std::path::Path>,
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

fn normalize_codex_models(
    models: Vec<CodexModelWire>,
) -> Result<Vec<CodexModelCapability>, CodexAdapterError> {
    models
        .into_iter()
        .filter(|model| !model.hidden)
        .take(MAX_CODEX_MODELS)
        .map(|model| {
            let reasoning_efforts = model
                .supported_reasoning_efforts
                .into_iter()
                .take(MAX_CODEX_REASONING_EFFORTS)
                .map(|effort| {
                    Ok(CodexReasoningEffortWire {
                        reasoning_effort: bounded(effort.reasoning_effort, 128)?,
                        description: bounded(effort.description, 512)?,
                    })
                })
                .collect::<Result<Vec<_>, CodexAdapterError>>()?;
            let default_reasoning_effort = bounded(model.default_reasoning_effort, 128)?;
            if !reasoning_efforts
                .iter()
                .any(|effort| effort.reasoning_effort == default_reasoning_effort)
            {
                return Err(CodexAdapterError::InvalidMessage(
                    "Codex model default effort is absent from its catalog".into(),
                ));
            }
            Ok(CodexModelCapability {
                model: bounded(model.model, 128)?,
                display_name: bounded(model.display_name, 192)?,
                description: bounded(model.description, 1024)?,
                default_reasoning_effort,
                reasoning_efforts,
                is_default: model.is_default,
            })
        })
        .collect()
}

fn normalize_codex_skills(
    response: Value,
) -> Result<Vec<AgentAvailableCommand>, CodexAdapterError> {
    let skills: CodexSkillsListWire =
        serde_json::from_value(response.get("result").cloned().ok_or_else(|| {
            CodexAdapterError::InvalidMessage("skills/list returned no result".into())
        })?)?;
    skills
        .data
        .into_iter()
        .flat_map(|group| group.skills)
        .filter(|skill| skill.enabled)
        .take(MAX_CODEX_SKILLS)
        .map(|skill| {
            let name = bounded(skill.name, 127)?;
            if name.is_empty()
                || name
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'\\' | b'$'))
            {
                return Err(CodexAdapterError::InvalidMessage(
                    "Codex skill name cannot be represented as a mention".into(),
                ));
            }
            Ok(AgentAvailableCommand {
                name: format!("${name}"),
                description: bounded(skill.description, 1024)?,
                input_hint: Some("Describe how this skill should help".into()),
            })
        })
        .collect()
}

fn normalize_codex_goal(wire: CodexThreadGoalWire) -> Result<AgentThreadGoal, CodexAdapterError> {
    let status = match wire.status.as_str() {
        "active" => AgentGoalStatus::Active,
        "paused" => AgentGoalStatus::Paused,
        "blocked" => AgentGoalStatus::Blocked,
        "usageLimited" => AgentGoalStatus::UsageLimited,
        "budgetLimited" => AgentGoalStatus::BudgetLimited,
        "complete" => AgentGoalStatus::Complete,
        _ => {
            return Err(CodexAdapterError::InvalidMessage(
                "thread goal returned an unknown status".into(),
            ));
        }
    };
    Ok(AgentThreadGoal {
        objective: bounded(wire.objective, 16 * 1024)?,
        status,
        token_budget: wire.token_budget.filter(|value| *value > 0),
        tokens_used: wire.tokens_used.max(0),
        time_used_seconds: wire.time_used_seconds.max(0),
    })
}

fn codex_goal_status_name(status: AgentGoalStatus) -> &'static str {
    match status {
        AgentGoalStatus::Active => "active",
        AgentGoalStatus::Paused => "paused",
        AgentGoalStatus::Blocked => "blocked",
        AgentGoalStatus::UsageLimited => "usageLimited",
        AgentGoalStatus::BudgetLimited => "budgetLimited",
        AgentGoalStatus::Complete => "complete",
    }
}

fn codex_config_options(
    catalog: &[CodexModelCapability],
    turn_config: &CodexTurnConfig,
) -> Result<Vec<AgentSessionConfigOption>, CodexAdapterError> {
    let selected = selected_codex_model(catalog, turn_config)?;
    let model_choices = catalog
        .iter()
        .map(|model| AgentSessionConfigChoice {
            value: model.model.clone(),
            name: model.display_name.clone(),
            description: Some(model.description.clone()),
            group: None,
        })
        .collect();
    let effort_choices = selected
        .reasoning_efforts
        .iter()
        .map(|effort| AgentSessionConfigChoice {
            value: effort.reasoning_effort.clone(),
            name: effort.reasoning_effort.clone(),
            description: Some(effort.description.clone()),
            group: None,
        })
        .collect();
    Ok(vec![
        AgentSessionConfigOption {
            id: "model".into(),
            name: "Model".into(),
            description: Some("Model used for the next Codex turn".into()),
            category: Some("model".into()),
            kind: AgentSessionConfigKind::Select {
                current_value: selected.model.clone(),
            },
            choices: model_choices,
        },
        AgentSessionConfigOption {
            id: "reasoning_effort".into(),
            name: "Reasoning".into(),
            description: Some("Reasoning effort used for the next Codex turn".into()),
            category: Some("thought_level".into()),
            kind: AgentSessionConfigKind::Select {
                current_value: turn_config
                    .reasoning_effort
                    .clone()
                    .unwrap_or_else(|| selected.default_reasoning_effort.clone()),
            },
            choices: effort_choices,
        },
    ])
}

fn selected_codex_model<'a>(
    catalog: &'a [CodexModelCapability],
    turn_config: &CodexTurnConfig,
) -> Result<&'a CodexModelCapability, CodexAdapterError> {
    let selected = turn_config
        .model
        .as_deref()
        .ok_or_else(|| CodexAdapterError::InvalidConfig("Codex model is not selected".into()))?;
    catalog
        .iter()
        .find(|model| model.model == selected)
        .ok_or_else(|| {
            CodexAdapterError::InvalidConfig("selected Codex model is unavailable".into())
        })
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

fn normalize_codex_plan(params: &Value) -> Result<Vec<AgentPlanEntry>, CodexAdapterError> {
    let entries = params
        .get("plan")
        .and_then(Value::as_array)
        .ok_or_else(|| CodexAdapterError::InvalidMessage("missing plan".into()))?;
    if entries.len() > MAX_CODEX_PLAN_ENTRIES {
        return Err(CodexAdapterError::InvalidMessage(
            "Codex plan exceeds 128 entries".into(),
        ));
    }
    entries
        .iter()
        .map(|entry| {
            let content = required_string(entry, "step")?;
            if content.is_empty() || content.len() > 16 * 1024 {
                return Err(CodexAdapterError::InvalidMessage(
                    "Codex plan step is empty or exceeds 16 KiB".into(),
                ));
            }
            let status = match entry.get("status").and_then(Value::as_str) {
                Some("pending") => AgentPlanStatus::Pending,
                Some("inProgress") => AgentPlanStatus::InProgress,
                Some("completed") => AgentPlanStatus::Completed,
                _ => {
                    return Err(CodexAdapterError::InvalidMessage(
                        "Codex plan step has an unsupported status".into(),
                    ));
                }
            };
            Ok(AgentPlanEntry {
                content,
                priority: AgentPlanPriority::Medium,
                status,
            })
        })
        .collect()
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
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn contained_fake_app_server_completes_initialization() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let runtime = temporary.path().join("runtime");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let executable = temporary.path().join("codex");
        fs::write(
            &executable,
            "#!/bin/sh\nwhile IFS= read -r line; do case \"$line\" in *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake\"}}';; esac; done\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let client = CodexAppServerClient::launch(CodexAppServerConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            implementation_version: "test".into(),
            workspace,
            codex_home: runtime.join("codex-home"),
            scratch_directory: runtime.join("scratch"),
            auth_file: None,
            brokered_mcp_server: None,
            containment: Some(AgentContainmentConfig {
                proxy_url: proxy_url.clone(),
                credentialed_proxy_url: proxy_url,
                allowed_hosts: vec!["api.openai.com".into()],
                allowed_unix_sockets: Vec::new(),
                read_paths: Vec::new(),
                write_paths: vec![runtime],
            }),
        })
        .unwrap();
        let response = client.initialize(Duration::from_secs(10)).unwrap();
        assert_eq!(response["result"]["userAgent"], "fake");
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }

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
    fn turn_plan_updates_become_bounded_plan_entries() {
        assert_eq!(
            normalize_codex_plan(&json!({
                "plan": [
                    {"step": "Inspect the repository", "status": "completed"},
                    {"step": "Review the architecture", "status": "inProgress"},
                    {"step": "Summarize the findings", "status": "pending"}
                ]
            }))
            .unwrap(),
            vec![
                AgentPlanEntry {
                    content: "Inspect the repository".into(),
                    priority: AgentPlanPriority::Medium,
                    status: AgentPlanStatus::Completed,
                },
                AgentPlanEntry {
                    content: "Review the architecture".into(),
                    priority: AgentPlanPriority::Medium,
                    status: AgentPlanStatus::InProgress,
                },
                AgentPlanEntry {
                    content: "Summarize the findings".into(),
                    priority: AgentPlanPriority::Medium,
                    status: AgentPlanStatus::Pending,
                },
            ]
        );
        assert!(
            normalize_codex_plan(&json!({
                "plan": [{"step": "Unknown", "status": "blocked"}]
            }))
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_codex_exposes_models_reasoning_and_skill_mentions() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let runtime = temporary.path().join("runtime");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let executable = temporary.path().join("codex");
        fs::write(
            &executable,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"userAgent":"fake"}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"gpt-a","displayName":"GPT A","description":"Fast model","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"low","description":"Low"},{"reasoningEffort":"medium","description":"Medium"}],"defaultReasoningEffort":"medium","isDefault":true},{"model":"gpt-b","displayName":"GPT B","description":"Deep model","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}],"defaultReasoningEffort":"high","isDefault":false}]}}' ;;
    *'"method":"skills/list"'*) printf '%s\n' '{"id":3,"result":{"data":[{"cwd":"workspace","skills":[{"name":"native-sdk","description":"Build Native UI","enabled":true},{"name":"disabled","description":"Hidden","enabled":false}],"errors":[]}]}}' ;;
    *'"method":"thread/start"'*) printf '%s\n' '{"id":4,"result":{"thread":{"id":"thread-1"}}}' ;;
    *'"method":"thread/goal/set"'*) printf '%s\n' '{"id":5,"result":{"goal":{"threadId":"thread-1","objective":"Ship the compact Agent UI","status":"active","tokenBudget":50000,"tokensUsed":1200,"timeUsedSeconds":90,"createdAt":1,"updatedAt":2}}}' ;;
    *'"method":"turn/start"'*'"model":"gpt-b"'*'"effort":"high"'*)
      printf '%s\n' '{"id":6,"result":{"turn":{"id":"turn-1"}}}'
      printf '%s\n' '{"method":"turn/plan/updated","params":{"threadId":"thread-1","turnId":"turn-1","explanation":"Inspect before editing","plan":[{"step":"Inspect the repository","status":"completed"},{"step":"Review the architecture","status":"inProgress"},{"step":"Summarize the findings","status":"pending"}]}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}}'
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let client = CodexAppServerClient::launch(CodexAppServerConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            implementation_version: "test".into(),
            workspace,
            codex_home: runtime.join("codex-home"),
            scratch_directory: runtime.join("scratch"),
            auth_file: None,
            brokered_mcp_server: None,
            containment: None,
        })
        .unwrap();

        // This fixture runs beside process-heavy PTY, Seatbelt, and gateway
        // tests in the full workspace suite. Keep the production caller-owned
        // deadline semantics, but do not make this integration fixture depend
        // on two seconds of otherwise unrelated host scheduler headroom.
        let fixture_timeout = Duration::from_secs(10);
        let thread_id = client.initialize_session(fixture_timeout).unwrap();
        assert_eq!(thread_id, "thread-1");
        let initial = client.session_capabilities().unwrap();
        assert_eq!(initial.config_options.len(), 2);
        assert_eq!(
            initial.available_commands,
            vec![
                AgentAvailableCommand {
                    name: "$native-sdk".into(),
                    description: "Build Native UI".into(),
                    input_hint: Some("Describe how this skill should help".into()),
                },
                AgentAvailableCommand {
                    name: "goal".into(),
                    description: "Set or clear a persistent Codex goal".into(),
                    input_hint: Some("Describe the goal, or type clear".into()),
                },
            ]
        );
        let updated = client
            .set_session_config_option(
                &thread_id,
                "model",
                AgentSessionConfigValue::Id {
                    value: "gpt-b".into(),
                },
                fixture_timeout,
            )
            .unwrap();
        assert_eq!(
            updated.config_options,
            vec![
                AgentSessionConfigOption {
                    id: "model".into(),
                    name: "Model".into(),
                    description: Some("Model used for the next Codex turn".into()),
                    category: Some("model".into()),
                    kind: AgentSessionConfigKind::Select {
                        current_value: "gpt-b".into(),
                    },
                    choices: vec![
                        AgentSessionConfigChoice {
                            value: "gpt-a".into(),
                            name: "GPT A".into(),
                            description: Some("Fast model".into()),
                            group: None,
                        },
                        AgentSessionConfigChoice {
                            value: "gpt-b".into(),
                            name: "GPT B".into(),
                            description: Some("Deep model".into()),
                            group: None,
                        },
                    ],
                },
                AgentSessionConfigOption {
                    id: "reasoning_effort".into(),
                    name: "Reasoning".into(),
                    description: Some("Reasoning effort used for the next Codex turn".into()),
                    category: Some("thought_level".into()),
                    kind: AgentSessionConfigKind::Select {
                        current_value: "high".into(),
                    },
                    choices: vec![AgentSessionConfigChoice {
                        value: "high".into(),
                        name: "high".into(),
                        description: Some("High".into()),
                        group: None,
                    }],
                },
            ]
        );
        assert_eq!(
            client
                .set_thread_goal(
                    &thread_id,
                    Some("Ship the compact Agent UI"),
                    Some(AgentGoalStatus::Active),
                    fixture_timeout,
                )
                .unwrap(),
            AgentThreadGoal {
                objective: "Ship the compact Agent UI".into(),
                status: AgentGoalStatus::Active,
                token_budget: Some(50_000),
                tokens_used: 1_200,
                time_used_seconds: 90,
            }
        );
        assert_eq!(client.thread_goal().unwrap().unwrap().tokens_used, 1_200);
        assert_eq!(
            client
                .start_turn(&thread_id, "Build it", fixture_timeout)
                .unwrap(),
            "turn-1"
        );
        assert_eq!(
            client.next_event(fixture_timeout).unwrap(),
            AgentDriverEvent::PlanUpdated {
                sequence: 7,
                thread_id: "thread-1".into(),
                turn_id: "turn-1".into(),
                entries: vec![
                    AgentPlanEntry {
                        content: "Inspect the repository".into(),
                        priority: AgentPlanPriority::Medium,
                        status: AgentPlanStatus::Completed,
                    },
                    AgentPlanEntry {
                        content: "Review the architecture".into(),
                        priority: AgentPlanPriority::Medium,
                        status: AgentPlanStatus::InProgress,
                    },
                    AgentPlanEntry {
                        content: "Summarize the findings".into(),
                        priority: AgentPlanPriority::Medium,
                        status: AgentPlanStatus::Pending,
                    },
                ],
            }
        );
        assert_eq!(
            client.next_event(fixture_timeout).unwrap(),
            AgentDriverEvent::TurnCompleted {
                sequence: 8,
                thread_id: "thread-1".into(),
                turn_id: Some("turn-1".into()),
                status: Some("completed".into()),
            }
        );
        assert_eq!(client.close().unwrap(), DriverState::Closed);
    }

    #[cfg(unix)]
    #[test]
    fn direct_codex_interrupts_active_turn_without_closing_thread() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let runtime = temporary.path().join("runtime");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let executable = temporary.path().join("codex");
        fs::write(
            &executable,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"userAgent":"fake"}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"gpt-test","displayName":"GPT Test","description":"Fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Medium"}],"defaultReasoningEffort":"medium","isDefault":true}]}}' ;;
    *'"method":"skills/list"'*) printf '%s\n' '{"id":3,"result":{"data":[]}}' ;;
    *'"method":"thread/start"'*) printf '%s\n' '{"id":4,"result":{"thread":{"id":"thread-cancel"}}}' ;;
    *'"method":"turn/start"'*) printf '%s\n' '{"id":5,"result":{"turn":{"id":"turn-cancel"}}}' ;;
    *'"method":"turn/interrupt"'*'"threadId":"thread-cancel"'*'"turnId":"turn-cancel"'*)
      printf '%s\n' '{"id":6,"result":{}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-cancel","turn":{"id":"turn-cancel","status":"interrupted"}}}' ;;
  esac
done
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let client = CodexAppServerClient::launch(CodexAppServerConfig {
            executable: executable.clone(),
            executable_sha256: sha256_file(&executable).unwrap(),
            implementation_version: "test".into(),
            workspace,
            codex_home: runtime.join("codex-home"),
            scratch_directory: runtime.join("scratch"),
            auth_file: None,
            brokered_mcp_server: None,
            containment: None,
        })
        .unwrap();
        let timeout = Duration::from_secs(10);
        let thread_id = client.initialize_session(timeout).unwrap();
        let turn_id = client
            .start_turn(&thread_id, "keep working", timeout)
            .unwrap();
        client.cancel_turn(&thread_id, &turn_id).unwrap();
        assert!(matches!(
            client.next_event(timeout).unwrap(),
            AgentDriverEvent::ProtocolNotice { .. }
        ));
        assert!(matches!(
            client.next_event(timeout).unwrap(),
            AgentDriverEvent::TurnCompleted { status: Some(status), .. } if status == "interrupted"
        ));
        assert_eq!(client.state().unwrap(), DriverState::Ready);
        assert_eq!(client.close().unwrap(), DriverState::Closed);
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
            stage_codex_auth_file(Some(&auth), &codex_home),
            Err(CodexAdapterError::InvalidConfig(_))
        ));

        let mut permissions = fs::metadata(&auth).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&auth, permissions).unwrap();
        let staged = stage_codex_auth_file(Some(&auth), &codex_home)
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
