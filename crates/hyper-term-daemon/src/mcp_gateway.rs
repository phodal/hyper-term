use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, bounded, select};
use hyper_term_drivers::{
    AgentEffectAuthorization, DenoGenUiCompiler, DenoGenUiConfig, DenoLspClient, DenoLspConfig,
    DriverState, ExternalRequestId, GenUiCompileRequest, MAX_MCP_FRAME_BYTES, McpAgentServer,
    McpAuthorizationOutcome, McpServerAction, McpToolCall, McpToolClass, McpToolResult,
    path_to_file_uri,
};
use hyper_term_protocol::{
    BrokeredMcpToolExecution, ControlRequest, ControlResponse, DomainEvent, OperationId,
    OperationOutcome, OperationState, PermissionDecision, TaskId, WireFrame,
};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{ControlClient, ControlClientError};

const AUTHORITY_TIMEOUT: Duration = Duration::from_secs(3);
const AUTHORITY_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_INPUT_CAPACITY: usize = 64;
const AUTHORITY_EVENT_CAPACITY: usize = 512;
const MAX_DIFF_OUTPUT_BYTES: usize = 800 * 1024;
const MAX_LSP_DOCUMENT_BYTES: u64 = 1024 * 1024;
const MAX_LSP_RESULT_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct DenoMcpExecutorConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub runtime_version: String,
    pub workspace_snapshot: PathBuf,
    pub cache_directory: PathBuf,
    pub scratch_directory: PathBuf,
}

#[derive(Clone, Debug)]
pub struct DenoGenUiMcpExecutorConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub runtime_version: String,
    pub compiler_script: PathBuf,
    pub compiler_script_sha256: String,
    pub compiler_wasm: PathBuf,
    pub compiler_wasm_sha256: String,
    pub compiler_version: String,
    pub cache_directory: PathBuf,
    pub scratch_directory: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct BrokeredMcpRuntimeConfig {
    pub deno_lsp: Option<DenoMcpExecutorConfig>,
    pub deno_genui: Option<DenoGenUiMcpExecutorConfig>,
}

#[derive(Clone, Debug)]
pub struct McpStdioConfig {
    socket: PathBuf,
    task_id: Option<TaskId>,
    deno_lsp_enabled: bool,
    deno_genui_enabled: bool,
}

impl McpStdioConfig {
    pub fn new(socket: PathBuf, agent_mode: bool) -> Result<Self, McpGatewayError> {
        if !agent_mode {
            return Err(McpGatewayError::AgentModeRequired);
        }
        if !socket.is_absolute() {
            return Err(McpGatewayError::SocketMustBeAbsolute);
        }
        Ok(Self {
            socket,
            task_id: None,
            deno_lsp_enabled: false,
            deno_genui_enabled: false,
        })
    }

    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn with_deno_lsp_enabled(mut self) -> Self {
        self.deno_lsp_enabled = true;
        self
    }

    pub fn with_deno_genui_enabled(mut self) -> Self {
        self.deno_genui_enabled = true;
        self
    }
}

pub fn run_mcp_stdio<R, W>(
    config: McpStdioConfig,
    input: R,
    output: &mut W,
) -> Result<(), McpGatewayError>
where
    R: Read + Send + 'static,
    W: Write,
{
    let (authority_events, observer) = spawn_authority_observer(&config.socket)?;
    let input_events = spawn_mcp_reader(input)?;
    let mut gateway = McpGateway::new(config, output);
    let result = loop {
        select! {
            recv(input_events) -> message => match message {
                Ok(InputEvent::Message(message)) => {
                    if let Err(error) = gateway.receive_mcp(message) {
                        break Err(error);
                    }
                }
                Ok(InputEvent::Closed) | Err(_) => break Ok(()),
                Ok(InputEvent::Failed(message)) => {
                    break Err(McpGatewayError::Input(message));
                }
            },
            recv(authority_events) -> message => match message {
                Ok(AuthorityEvent::Response(response)) => {
                    if let Err(error) = gateway.receive_authority(*response) {
                        break Err(error);
                    }
                }
                Ok(AuthorityEvent::Failed(message)) => {
                    break Err(McpGatewayError::AuthorityObserver(message));
                }
                Err(_) => break Err(McpGatewayError::AuthorityObserver(
                    "authority observer disconnected".into()
                )),
            }
        }
    };
    drop(observer);
    result
}

struct McpGateway<'a, W> {
    socket: PathBuf,
    output: &'a mut W,
    server: McpAgentServer,
    task_id: Option<TaskId>,
    pending: HashMap<OperationId, PendingAuthorityCall>,
}

impl<'a, W: Write> McpGateway<'a, W> {
    fn new(config: McpStdioConfig, output: &'a mut W) -> Self {
        let mut tools = vec![McpToolClass::DiffReview];
        if config.deno_lsp_enabled {
            tools.push(McpToolClass::DenoLspQuery);
        }
        if config.deno_genui_enabled {
            tools.push(McpToolClass::GenUiCompile);
        }
        Self {
            socket: config.socket,
            output,
            server: McpAgentServer::with_tools(Uuid::new_v4(), tools),
            task_id: config.task_id,
            pending: HashMap::new(),
        }
    }

    fn receive_mcp(&mut self, message: Value) -> Result<(), McpGatewayError> {
        match self.server.receive(message)? {
            Some(McpServerAction::Response(response)) => self.write_response(&response),
            Some(McpServerAction::ToolProposed(call)) => self.propose_tool(*call),
            None => Ok(()),
        }
    }

    fn propose_tool(&mut self, call: McpToolCall) -> Result<(), McpGatewayError> {
        let task_id = self.task()?;
        let response = authority_request(
            &self.socket,
            ControlRequest::ProposeBrokeredMcpTool {
                task_id,
                tool_name: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let response = self.server.fail_tool(
                    &call.request_id,
                    format!("permission broker failed: {error}"),
                )?;
                return self.write_response(&response);
            }
        };
        match response {
            ControlResponse::OperationUpdated {
                operation_id,
                revision,
                state: OperationState::WaitingHuman,
            } => {
                self.pending.insert(
                    operation_id,
                    PendingAuthorityCall {
                        request_id: call.request_id,
                        proposal_digest: call.proposal.payload_sha256,
                        waiting_revision: revision,
                        decision: None,
                    },
                );
                Ok(())
            }
            ControlResponse::Error { code, message } => {
                let response = self.server.fail_tool(
                    &call.request_id,
                    format!("permission broker rejected the proposal ({code}): {message}"),
                )?;
                self.write_response(&response)
            }
            response => {
                let response = self.server.fail_tool(
                    &call.request_id,
                    format!("permission broker returned an unexpected response: {response:?}"),
                )?;
                self.write_response(&response)
            }
        }
    }

    fn receive_authority(&mut self, response: ControlResponse) -> Result<(), McpGatewayError> {
        let ControlResponse::Event { event } = response else {
            return Ok(());
        };
        let Some(operation_id) = event.operation_id else {
            return Ok(());
        };
        if !self.pending.contains_key(&operation_id) {
            return Ok(());
        }
        match event.payload {
            DomainEvent::PermissionDecided { decision, .. } => {
                if let Some(pending) = self.pending.get_mut(&operation_id) {
                    pending.decision = Some(decision);
                }
                Ok(())
            }
            DomainEvent::OperationStateChanged {
                revision,
                to: OperationState::Authorized,
                ..
            } => self.execute_authorized(operation_id, revision),
            DomainEvent::OperationStateChanged {
                to: OperationState::Cancelled,
                ..
            } => self.reject_cancelled(operation_id),
            _ => Ok(()),
        }
    }

    fn execute_authorized(
        &mut self,
        operation_id: OperationId,
        authorized_revision: u64,
    ) -> Result<(), McpGatewayError> {
        let pending = self
            .pending
            .get(&operation_id)
            .cloned()
            .ok_or(McpGatewayError::UnknownOperation(operation_id))?;
        if pending.waiting_revision + 1 != authorized_revision {
            return Err(McpGatewayError::UnexpectedOperationRevision {
                expected: pending.waiting_revision + 1,
                actual: authorized_revision,
            });
        }
        let authorization = AgentEffectAuthorization {
            operation_id,
            operation_revision: authorized_revision,
            proposal_sha256: pending.proposal_digest,
            decision: pending.decision.unwrap_or(PermissionDecision::AllowOnce),
        };
        let McpAuthorizationOutcome::Authorized(call) = self
            .server
            .authorize_tool(&pending.request_id, authorization)?
        else {
            return Err(McpGatewayError::AuthorizedOperationWasRejected);
        };
        let task_id = self.task_id.ok_or(McpGatewayError::TaskMissing)?;
        match authority_request(
            &self.socket,
            ControlRequest::RunAuthorizedBrokeredMcpTool {
                task_id,
                operation_id,
                expected_revision: authorized_revision,
                tool_name: call.name.clone(),
                proposal_digest: call.proposal.payload_sha256.clone(),
                arguments: call.arguments.clone(),
            },
        )? {
            ControlResponse::BrokeredMcpToolExecuted { execution } => {
                let result = McpToolResult {
                    text: execution.text,
                    structured_content: execution.structured_content,
                    is_error: execution.is_error,
                };
                let response = self.server.complete_tool(&pending.request_id, result)?;
                self.pending.remove(&operation_id);
                self.write_response(&response)
            }
            ControlResponse::Error { code, message } => self.fail_authorized_call(
                operation_id,
                &pending.request_id,
                format!("Rust authority could not run the tool ({code}): {message}"),
            ),
            response => self.fail_authorized_call(
                operation_id,
                &pending.request_id,
                format!("Rust authority returned an unexpected response: {response:?}"),
            ),
        }
    }

    fn reject_cancelled(&mut self, operation_id: OperationId) -> Result<(), McpGatewayError> {
        let pending = self
            .pending
            .remove(&operation_id)
            .ok_or(McpGatewayError::UnknownOperation(operation_id))?;
        let outcome = self.server.authorize_tool(
            &pending.request_id,
            AgentEffectAuthorization {
                operation_id,
                operation_revision: pending.waiting_revision,
                proposal_sha256: pending.proposal_digest,
                decision: pending.decision.unwrap_or(PermissionDecision::Cancelled),
            },
        )?;
        let McpAuthorizationOutcome::Rejected(response) = outcome else {
            return Err(McpGatewayError::CancelledOperationWasAuthorized);
        };
        self.write_response(&response)
    }

    fn fail_authorized_call(
        &mut self,
        operation_id: OperationId,
        request_id: &ExternalRequestId,
        message: String,
    ) -> Result<(), McpGatewayError> {
        let response = self.server.complete_tool(
            request_id,
            McpToolResult {
                text: message,
                structured_content: None,
                is_error: true,
            },
        )?;
        self.pending.remove(&operation_id);
        self.write_response(&response)
    }

    fn task(&mut self) -> Result<TaskId, McpGatewayError> {
        if let Some(task_id) = self.task_id {
            return Ok(task_id);
        }
        let task_id = match authority_request(
            &self.socket,
            ControlRequest::CreateTask {
                title: "Agent MCP tools".into(),
            },
        )? {
            ControlResponse::TaskCreated { task_id } => task_id,
            ControlResponse::Error { code, message } => {
                return Err(McpGatewayError::AuthorityRejected { code, message });
            }
            response => return Err(McpGatewayError::UnexpectedAuthorityResponse(response)),
        };
        self.task_id = Some(task_id);
        Ok(task_id)
    }

    fn write_response(&mut self, response: &Value) -> Result<(), McpGatewayError> {
        hyper_term_drivers::DriverFraming::JsonLines.write(
            &mut self.output,
            response,
            MAX_MCP_FRAME_BYTES,
        )?;
        Ok(())
    }
}

#[derive(Clone)]
struct ToolExecution {
    result: McpToolResult,
    outcome: OperationOutcome,
}

impl From<BrokeredMcpToolExecution> for ToolExecution {
    fn from(execution: BrokeredMcpToolExecution) -> Self {
        Self {
            result: McpToolResult {
                text: execution.text,
                structured_content: execution.structured_content,
                is_error: execution.is_error,
            },
            outcome: execution.outcome,
        }
    }
}

impl ToolExecution {
    fn definitive(result: McpToolResult) -> Self {
        let outcome = if result.is_error {
            OperationOutcome::Failed
        } else {
            OperationOutcome::Succeeded
        };
        Self { result, outcome }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            result: tool_failure(message),
            outcome: OperationOutcome::Failed,
        }
    }

    fn from_driver_result(
        result: Result<McpToolResult, String>,
        state: DriverState,
        label: &str,
    ) -> Self {
        match result {
            Ok(result) => Self::definitive(result),
            Err(error) => Self {
                result: tool_failure(format!("{label} failed: {error}")),
                outcome: if state == DriverState::UnknownExecution {
                    OperationOutcome::UnknownExecution
                } else {
                    OperationOutcome::Failed
                },
            },
        }
    }
}

#[derive(Clone)]
struct PendingAuthorityCall {
    request_id: ExternalRequestId,
    proposal_digest: String,
    waiting_revision: u64,
    decision: Option<PermissionDecision>,
}

fn authority_request(
    socket: &Path,
    request: ControlRequest,
) -> Result<ControlResponse, McpGatewayError> {
    let request_timeout = authority_request_timeout(&request);
    let mut client = ControlClient::connect(socket, AUTHORITY_TIMEOUT)?;
    let response = client.request(request, request_timeout)?;
    match response {
        ControlResponse::Error { code, message } => {
            Err(McpGatewayError::AuthorityRejected { code, message })
        }
        response => Ok(response),
    }
}

fn authority_request_timeout(request: &ControlRequest) -> Duration {
    if matches!(request, ControlRequest::RunAuthorizedBrokeredMcpTool { .. }) {
        AUTHORITY_EXECUTION_TIMEOUT
    } else {
        AUTHORITY_TIMEOUT
    }
}

fn tool_failure(message: impl Into<String>) -> McpToolResult {
    McpToolResult {
        text: message.into(),
        structured_content: None,
        is_error: true,
    }
}

pub(crate) struct BrokeredMcpExecutor {
    deno_lsp_config: Option<DenoMcpExecutorConfig>,
    deno_lsp: Option<DenoLspExecutor>,
    deno_genui_config: Option<DenoGenUiMcpExecutorConfig>,
    deno_genui: Option<DenoGenUiExecutor>,
}

impl BrokeredMcpExecutor {
    pub(crate) fn new(config: BrokeredMcpRuntimeConfig) -> Result<Self, McpGatewayError> {
        if let Some(config) = &config.deno_lsp {
            if !config.executable.is_absolute()
                || !config.workspace_snapshot.is_absolute()
                || !config.cache_directory.is_absolute()
                || !config.scratch_directory.is_absolute()
            {
                return Err(McpGatewayError::DenoPathsMustBeAbsolute);
            }
            if config.runtime_version.is_empty() || !is_sha256(&config.executable_sha256) {
                return Err(McpGatewayError::InvalidDenoManifest);
            }
        }
        if let Some(config) = &config.deno_genui {
            if !config.executable.is_absolute()
                || !config.compiler_script.is_absolute()
                || !config.compiler_wasm.is_absolute()
                || !config.cache_directory.is_absolute()
                || !config.scratch_directory.is_absolute()
            {
                return Err(McpGatewayError::DenoPathsMustBeAbsolute);
            }
            if config.runtime_version.is_empty()
                || config.compiler_version.is_empty()
                || !is_sha256(&config.executable_sha256)
                || !is_sha256(&config.compiler_script_sha256)
                || !is_sha256(&config.compiler_wasm_sha256)
            {
                return Err(McpGatewayError::InvalidDenoManifest);
            }
        }
        Ok(Self {
            deno_lsp_config: config.deno_lsp,
            deno_lsp: None,
            deno_genui_config: config.deno_genui,
            deno_genui: None,
        })
    }

    pub(crate) fn execute(
        &mut self,
        tool_name: &str,
        arguments: &Value,
    ) -> BrokeredMcpToolExecution {
        let execution = match tool_name {
            "hyper_term.diff.review" => ToolExecution::definitive(diff_review(arguments)),
            "hyper_term.lsp.query" => self.execute_lsp(arguments),
            "hyper_term.genui.compile" => self.execute_genui(arguments),
            _ => ToolExecution::failed("tool is not a brokered Rust executor"),
        };
        BrokeredMcpToolExecution {
            text: execution.result.text,
            structured_content: execution.result.structured_content,
            is_error: execution.result.is_error,
            outcome: execution.outcome,
        }
    }

    fn execute_lsp(&mut self, arguments: &Value) -> ToolExecution {
        if self.deno_lsp.is_none() {
            let Some(config) = self.deno_lsp_config.clone() else {
                return ToolExecution::failed("Deno LSP executor is not configured");
            };
            match DenoLspExecutor::launch(config) {
                Ok(executor) => self.deno_lsp = Some(executor),
                Err(error) => {
                    return ToolExecution::failed(format!("Deno LSP could not start: {error}"));
                }
            }
        }
        let (result, state) = {
            let executor = self.deno_lsp.as_mut().expect("executor was initialized");
            let result = executor.query(arguments);
            let state = executor.client.state().unwrap_or(DriverState::Failed);
            (result, state)
        };
        if state.is_terminal() {
            self.deno_lsp = None;
        }
        ToolExecution::from_driver_result(result, state, "Deno LSP query")
    }

    fn execute_genui(&mut self, arguments: &Value) -> ToolExecution {
        if self.deno_genui.is_none() {
            let Some(config) = self.deno_genui_config.clone() else {
                return ToolExecution::failed("GenUI compiler executor is not configured");
            };
            match DenoGenUiExecutor::launch(config) {
                Ok(executor) => self.deno_genui = Some(executor),
                Err(error) => {
                    return ToolExecution::failed(format!(
                        "GenUI compiler could not start: {error}"
                    ));
                }
            }
        }
        let (result, state) = {
            let executor = self.deno_genui.as_mut().expect("executor was initialized");
            let result = executor.compile(arguments);
            let state = executor.compiler.state().unwrap_or(DriverState::Failed);
            (result, state)
        };
        if state.is_terminal() {
            self.deno_genui = None;
        }
        ToolExecution::from_driver_result(result, state, "GenUI compile")
    }
}

struct DenoLspExecutor {
    client: DenoLspClient,
    workspace_snapshot: PathBuf,
    opened_documents: HashMap<PathBuf, i32>,
}

struct DenoGenUiExecutor {
    compiler: DenoGenUiCompiler,
    next_source_revision: u64,
}

impl DenoGenUiExecutor {
    fn launch(config: DenoGenUiMcpExecutorConfig) -> Result<Self, String> {
        let compiler = DenoGenUiCompiler::launch(
            DenoGenUiConfig {
                executable: config.executable,
                executable_sha256: config.executable_sha256,
                runtime_version: config.runtime_version,
                compiler_script: config.compiler_script,
                compiler_script_sha256: config.compiler_script_sha256,
                compiler_wasm: config.compiler_wasm,
                compiler_wasm_sha256: config.compiler_wasm_sha256,
                compiler_version: config.compiler_version,
                cache_directory: config.cache_directory,
                scratch_directory: config.scratch_directory,
            },
            Duration::from_secs(10),
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            compiler,
            next_source_revision: 1,
        })
    }

    fn compile(&mut self, arguments: &Value) -> Result<McpToolResult, String> {
        let source = arguments["source"]
            .as_str()
            .ok_or_else(|| "source is missing".to_owned())?;
        let entrypoint = normalize_genui_entry(arguments.get("entry").and_then(Value::as_str))?;
        let source_revision = self.next_source_revision;
        self.next_source_revision = self
            .next_source_revision
            .checked_add(1)
            .ok_or_else(|| "GenUI source revision exhausted".to_owned())?;
        let source_files = BTreeMap::from([(entrypoint.clone(), source.to_owned())]);
        let candidate = self
            .compiler
            .compile(
                GenUiCompileRequest {
                    source_revision,
                    entrypoint: entrypoint.clone(),
                    files: source_files.clone(),
                },
                Duration::from_secs(15),
            )
            .map_err(|error| error.to_string())?;
        let mut structured = serde_json::to_value(&candidate).map_err(|error| error.to_string())?;
        structured["source_files"] =
            serde_json::to_value(source_files).map_err(|error| error.to_string())?;
        Ok(McpToolResult::success(
            format!(
                "Compiled GenUI revision {} with {} {} (artifact {}).",
                candidate.source_revision,
                candidate.compiler.name,
                candidate.compiler.version,
                candidate.content_digest
            ),
            Some(structured),
        ))
    }
}

fn normalize_genui_entry(entry: Option<&str>) -> Result<String, String> {
    let entry = entry
        .filter(|value| !value.is_empty())
        .unwrap_or("/App.tsx");
    let entry = if entry.starts_with('/') {
        entry.to_owned()
    } else {
        format!("/{entry}")
    };
    if entry.contains('\\')
        || entry.contains("..")
        || ![".tsx", ".ts", ".jsx", ".js"]
            .iter()
            .any(|extension| entry.ends_with(extension))
    {
        return Err("entry must be a bounded virtual TS/JS module path".into());
    }
    Ok(entry)
}

impl DenoLspExecutor {
    fn launch(config: DenoMcpExecutorConfig) -> Result<Self, String> {
        let workspace_snapshot = config
            .workspace_snapshot
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let client = DenoLspClient::launch(DenoLspConfig {
            executable: config.executable,
            executable_sha256: config.executable_sha256,
            runtime_version: config.runtime_version,
            workspace_snapshot: workspace_snapshot.clone(),
            cache_directory: config.cache_directory,
            scratch_directory: config.scratch_directory,
            config_file: None,
        })
        .map_err(|error| error.to_string())?;
        client
            .initialize(Duration::from_secs(10))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            client,
            workspace_snapshot,
            opened_documents: HashMap::new(),
        })
    }

    fn query(&mut self, arguments: &Value) -> Result<McpToolResult, String> {
        let method = arguments["method"]
            .as_str()
            .ok_or_else(|| "method is missing".to_owned())?;
        let relative = arguments["documentPath"]
            .as_str()
            .ok_or_else(|| "documentPath is missing".to_owned())?;
        let relative = Path::new(relative);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("documentPath must stay within the workspace snapshot".into());
        }
        let document = self
            .workspace_snapshot
            .join(relative)
            .canonicalize()
            .map_err(|_| "document does not exist in the workspace snapshot".to_owned())?;
        if !document.starts_with(&self.workspace_snapshot) || !document.is_file() {
            return Err("documentPath escapes the workspace snapshot".into());
        }
        let metadata = fs::metadata(&document).map_err(|error| error.to_string())?;
        if metadata.len() > MAX_LSP_DOCUMENT_BYTES {
            return Err(format!(
                "document exceeds the {MAX_LSP_DOCUMENT_BYTES}-byte LSP bound"
            ));
        }
        let uri = path_to_file_uri(&document).map_err(|error| error.to_string())?;
        if !self.opened_documents.contains_key(&document) {
            let source = fs::read_to_string(&document)
                .map_err(|_| "document is not bounded UTF-8 source".to_owned())?;
            self.client
                .notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id(&document),
                            "version": 1,
                            "text": source
                        }
                    }),
                )
                .map_err(|error| error.to_string())?;
            self.opened_documents.insert(document.clone(), 1);
        }
        let text_document = json!({"uri": uri});
        let position = arguments
            .get("position")
            .cloned()
            .unwrap_or_else(|| json!({"line": 0, "character": 0}));
        let params = match method {
            "textDocument/documentSymbol" => json!({"textDocument": text_document}),
            "textDocument/formatting" => json!({
                "textDocument": text_document,
                "options": {"tabSize": 2, "insertSpaces": true}
            }),
            "textDocument/references" => json!({
                "textDocument": text_document,
                "position": position,
                "context": {
                    "includeDeclaration": arguments["includeDeclaration"].as_bool().unwrap_or(true)
                }
            }),
            _ => json!({"textDocument": text_document, "position": position}),
        };
        let response = self
            .client
            .request(method, params, Duration::from_secs(10))
            .map_err(|error| error.to_string())?;
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        let structured = json!({
            "method": method,
            "documentPath": relative.to_string_lossy(),
            "result": result
        });
        let text = serde_json::to_string_pretty(&structured).map_err(|error| error.to_string())?;
        if text.len() > MAX_LSP_RESULT_BYTES {
            return Err(format!(
                "LSP result exceeds the {MAX_LSP_RESULT_BYTES}-byte result bound"
            ));
        }
        Ok(McpToolResult::success(text, Some(structured)))
    }
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ts") | Some("mts") | Some("cts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("json") | Some("jsonc") => "json",
        _ => "plaintext",
    }
}

fn diff_review(arguments: &Value) -> McpToolResult {
    let before = arguments["before"].as_str().unwrap_or_default();
    let after = arguments["after"].as_str().unwrap_or_default();
    if before == after {
        return McpToolResult::success(
            "No changes.",
            Some(json!({
                "changed": false,
                "beforeLines": before.lines().count(),
                "afterLines": after.lines().count(),
                "unifiedDiff": ""
            })),
        );
    }
    let before_lines = before.lines().collect::<Vec<_>>();
    let after_lines = after.lines().collect::<Vec<_>>();
    let prefix = before_lines
        .iter()
        .zip(&after_lines)
        .take_while(|(left, right)| left == right)
        .count();
    let remaining_before = &before_lines[prefix..];
    let remaining_after = &after_lines[prefix..];
    let suffix = remaining_before
        .iter()
        .rev()
        .zip(remaining_after.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let before_end = before_lines.len().saturating_sub(suffix);
    let after_end = after_lines.len().saturating_sub(suffix);
    let removed = &before_lines[prefix..before_end];
    let added = &after_lines[prefix..after_end];
    let mut unified = format!(
        "--- before\n+++ after\n@@ -{},{} +{},{} @@\n",
        prefix + 1,
        removed.len(),
        prefix + 1,
        added.len()
    );
    for line in removed {
        let _ = writeln!(unified, "-{line}");
    }
    for line in added {
        let _ = writeln!(unified, "+{line}");
    }
    if unified.len() > MAX_DIFF_OUTPUT_BYTES {
        return McpToolResult {
            text: format!("diff output exceeds the {MAX_DIFF_OUTPUT_BYTES}-byte result bound"),
            structured_content: None,
            is_error: true,
        };
    }
    McpToolResult::success(
        unified.clone(),
        Some(json!({
            "changed": true,
            "beforeLines": before_lines.len(),
            "afterLines": after_lines.len(),
            "removedLines": removed.len(),
            "addedLines": added.len(),
            "unifiedDiff": unified
        })),
    )
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

enum InputEvent {
    Message(Value),
    Closed,
    Failed(String),
}

fn spawn_mcp_reader(
    input: impl Read + Send + 'static,
) -> Result<Receiver<InputEvent>, McpGatewayError> {
    let (sender, receiver) = bounded(MCP_INPUT_CAPACITY);
    thread::Builder::new()
        .name("hyper-term-mcp-stdin".into())
        .spawn(move || {
            let mut reader = BufReader::new(input);
            loop {
                match hyper_term_drivers::DriverFraming::JsonLines
                    .read(&mut reader, MAX_MCP_FRAME_BYTES)
                {
                    Ok(Some(message)) => {
                        if sender.send(InputEvent::Message(message)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(InputEvent::Closed);
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(InputEvent::Failed(error.to_string()));
                        break;
                    }
                }
            }
        })?;
    Ok(receiver)
}

enum AuthorityEvent {
    Response(Box<ControlResponse>),
    Failed(String),
}

struct AuthorityObserver {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for AuthorityObserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn_authority_observer(
    socket: &Path,
) -> Result<(Receiver<AuthorityEvent>, AuthorityObserver), McpGatewayError> {
    let mut client = ControlClient::connect(socket, AUTHORITY_TIMEOUT)?;
    let (sender, receiver) = bounded(AUTHORITY_EVENT_CAPACITY);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("hyper-term-mcp-authority".into())
        .spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match client.recv_timeout(Duration::from_millis(100)) {
                    Ok(WireFrame::Response(response)) => {
                        if sender
                            .send(AuthorityEvent::Response(Box::new(response.response)))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) | Err(ControlClientError::Timeout) => {}
                    Err(error) => {
                        let _ = sender.send(AuthorityEvent::Failed(error.to_string()));
                        break;
                    }
                }
            }
        })?;
    Ok((
        receiver,
        AuthorityObserver {
            stop,
            thread: Some(thread),
        },
    ))
}

#[derive(Debug, Error)]
pub enum McpGatewayError {
    #[error("MCP gateway I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Control(#[from] ControlClientError),
    #[error(transparent)]
    Driver(#[from] hyper_term_drivers::DriverError),
    #[error(transparent)]
    Mcp(#[from] hyper_term_drivers::McpServerError),
    #[error("MCP gateway JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("MCP stdio reader failed: {0}")]
    Input(String),
    #[error("MCP authority observer failed: {0}")]
    AuthorityObserver(String),
    #[error("MCP tools are available only in explicit Agent mode")]
    AgentModeRequired,
    #[error("MCP authority socket must be absolute")]
    SocketMustBeAbsolute,
    #[error("Deno MCP executor paths must be absolute")]
    DenoPathsMustBeAbsolute,
    #[error("Deno MCP executor manifest is incomplete")]
    InvalidDenoManifest,
    #[error("hyperd rejected the MCP operation ({code}): {message}")]
    AuthorityRejected { code: String, message: String },
    #[error("hyperd returned an unexpected MCP response: {0:?}")]
    UnexpectedAuthorityResponse(ControlResponse),
    #[error("MCP authority event referenced unknown operation {0}")]
    UnknownOperation(OperationId),
    #[error("MCP operation revision mismatch: expected {expected}, got {actual}")]
    UnexpectedOperationRevision { expected: u64, actual: u64 },
    #[error("authorized MCP operation unexpectedly produced a rejection")]
    AuthorizedOperationWasRejected,
    #[error("cancelled MCP operation unexpectedly produced an authorization")]
    CancelledOperationWasAuthorized,
    #[error("MCP gateway has not created its authority task")]
    TaskMissing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_tool_execution_uses_the_bounded_runtime_deadline() {
        let request = ControlRequest::RunAuthorizedBrokeredMcpTool {
            task_id: TaskId::new(),
            operation_id: OperationId::new(),
            expected_revision: 3,
            tool_name: "hyper_term.genui.compile".into(),
            proposal_digest: "0".repeat(64),
            arguments: json!({"source": "export default 1"}),
        };

        assert_eq!(
            authority_request_timeout(&request),
            AUTHORITY_EXECUTION_TIMEOUT
        );
        assert!(AUTHORITY_EXECUTION_TIMEOUT > AUTHORITY_TIMEOUT);
    }

    #[test]
    fn diff_review_emits_a_bounded_single_hunk() {
        let result = diff_review(&json!({
            "before": "one\ntwo\nthree\n",
            "after": "one\nsecond\nthree\n"
        }));
        assert!(!result.is_error);
        assert!(result.text.contains("@@ -2,1 +2,1 @@"));
        assert!(result.text.contains("-two"));
        assert!(result.text.contains("+second"));
        assert_eq!(result.structured_content.unwrap()["removedLines"], 1);
    }

    #[test]
    fn terminal_mode_cannot_construct_an_mcp_gateway() {
        assert!(matches!(
            McpStdioConfig::new(PathBuf::from("/tmp/hyperd.sock"), false),
            Err(McpGatewayError::AgentModeRequired)
        ));
    }

    #[test]
    fn genui_entry_is_normalized_to_a_bounded_virtual_module() {
        assert_eq!(normalize_genui_entry(None).unwrap(), "/App.tsx");
        assert_eq!(
            normalize_genui_entry(Some("Panel.tsx")).unwrap(),
            "/Panel.tsx"
        );
        assert!(normalize_genui_entry(Some("../secret.ts")).is_err());
        assert!(normalize_genui_entry(Some("App.txt")).is_err());
    }

    #[test]
    fn unknown_driver_execution_is_not_collapsed_into_failure() {
        let execution = ToolExecution::from_driver_result(
            Err("deadline exceeded".into()),
            DriverState::UnknownExecution,
            "GenUI compile",
        );
        assert_eq!(execution.outcome, OperationOutcome::UnknownExecution);
        assert!(execution.result.is_error);
        assert!(execution.result.text.contains("deadline exceeded"));

        let failed = ToolExecution::from_driver_result(
            Err("could not launch".into()),
            DriverState::Failed,
            "Deno LSP query",
        );
        assert_eq!(failed.outcome, OperationOutcome::Failed);
    }
}
