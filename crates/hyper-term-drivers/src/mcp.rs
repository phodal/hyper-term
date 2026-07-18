use std::collections::HashMap;

use hyper_term_protocol::PermissionDecision;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    AgentEffectAuthorization, AgentEffectKind, AgentEffectProposal, ExternalRequestId,
    StructuredAgentProtocol,
};

pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_MCP_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_MCP_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_MCP_RESULT_BYTES: usize = 1024 * 1024;
const MAX_PENDING_TOOL_CALLS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum McpLifecycle {
    New,
    Initializing,
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolClass {
    GenUiCompile,
    DenoLspQuery,
    DiffReview,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpToolCall {
    pub request_id: ExternalRequestId,
    pub class: McpToolClass,
    pub name: String,
    pub arguments: Value,
    pub proposal: AgentEffectProposal,
}

#[derive(Clone, Debug, PartialEq)]
pub enum McpServerAction {
    Response(Value),
    ToolProposed(Box<McpToolCall>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpToolResult {
    pub text: String,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl McpToolResult {
    pub fn success(text: impl Into<String>, structured_content: Option<Value>) -> Self {
        Self {
            text: text.into(),
            structured_content,
            is_error: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum McpAuthorizationOutcome {
    Authorized(Box<McpToolCall>),
    Rejected(Value),
}

#[derive(Clone, Debug)]
struct PendingToolCall {
    call: McpToolCall,
    authorized: bool,
}

/// Agent-mode MCP protocol state. This adapter never executes a tool itself:
/// every `tools/call` becomes an effect proposal that the Rust permission
/// broker must authorize before an executor can receive it.
pub struct McpAgentServer {
    driver_id: Uuid,
    lifecycle: McpLifecycle,
    enabled_tools: Vec<McpToolClass>,
    pending: HashMap<ExternalRequestId, PendingToolCall>,
}

impl McpAgentServer {
    pub fn new(driver_id: Uuid) -> Self {
        Self::with_tools(
            driver_id,
            [
                McpToolClass::GenUiCompile,
                McpToolClass::DenoLspQuery,
                McpToolClass::DiffReview,
            ],
        )
    }

    pub fn with_tools(driver_id: Uuid, tools: impl IntoIterator<Item = McpToolClass>) -> Self {
        let mut enabled_tools = Vec::new();
        for tool in tools {
            if !enabled_tools.contains(&tool) {
                enabled_tools.push(tool);
            }
        }
        Self {
            driver_id,
            lifecycle: McpLifecycle::New,
            enabled_tools,
            pending: HashMap::new(),
        }
    }

    pub fn receive(&mut self, message: Value) -> Result<Option<McpServerAction>, McpServerError> {
        ensure_size(&message, MAX_MCP_FRAME_BYTES, "MCP frame")?;
        let id = rpc_id(message.get("id"));
        if message.get("jsonrpc") != Some(&Value::String("2.0".into())) {
            return Ok(id.map(|id| {
                McpServerAction::Response(rpc_error(id, -32600, "JSON-RPC version must be 2.0"))
            }));
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(id.map(|id| {
                McpServerAction::Response(rpc_error(id, -32600, "request method is missing"))
            }));
        };

        match method {
            "initialize" => self.initialize(id, message.get("params")),
            "notifications/initialized" => self.mark_initialized(id),
            "ping" => Ok(id.map(|id| McpServerAction::Response(rpc_result(id, json!({}))))),
            "tools/list" => self.list_tools(id, message.get("params")),
            "tools/call" => self.call_tool(id, message.get("params")),
            "resources/list" => self.empty_inventory(id, "resources"),
            "resources/templates/list" => self.empty_inventory(id, "resourceTemplates"),
            _ => {
                Ok(id
                    .map(|id| McpServerAction::Response(rpc_error(id, -32601, "method not found"))))
            }
        }
    }

    pub fn pending_calls(&self) -> Vec<McpToolCall> {
        self.pending
            .values()
            .map(|pending| pending.call.clone())
            .collect()
    }

    pub fn authorize_tool(
        &mut self,
        request_id: &ExternalRequestId,
        authorization: AgentEffectAuthorization,
    ) -> Result<McpAuthorizationOutcome, McpServerError> {
        if authorization.operation_revision == 0 {
            return Err(McpServerError::InvalidAuthorization(
                "operation revision must be positive".into(),
            ));
        }
        let pending = self
            .pending
            .get_mut(request_id)
            .ok_or(McpServerError::UnknownToolCall)?;
        if authorization.proposal_sha256 != pending.call.proposal.payload_sha256 {
            return Err(McpServerError::InvalidAuthorization(
                "proposal digest does not match the pending tool call".into(),
            ));
        }
        match authorization.decision {
            PermissionDecision::AllowOnce => {
                if pending.authorized {
                    return Err(McpServerError::AlreadyAuthorized);
                }
                pending.authorized = true;
                Ok(McpAuthorizationOutcome::Authorized(Box::new(
                    pending.call.clone(),
                )))
            }
            PermissionDecision::RejectOnce | PermissionDecision::Cancelled => {
                let pending = self
                    .pending
                    .remove(request_id)
                    .ok_or(McpServerError::UnknownToolCall)?;
                let reason = if authorization.decision == PermissionDecision::Cancelled {
                    "tool call was cancelled"
                } else {
                    "tool call was rejected by the permission broker"
                };
                Ok(McpAuthorizationOutcome::Rejected(tool_result(
                    &pending.call.request_id,
                    McpToolResult {
                        text: reason.into(),
                        structured_content: None,
                        is_error: true,
                    },
                )?))
            }
            PermissionDecision::AllowAlways | PermissionDecision::RejectAlways => {
                Err(McpServerError::InvalidAuthorization(
                    "persistent policy decisions are not wire-level authorizations".into(),
                ))
            }
        }
    }

    pub fn complete_tool(
        &mut self,
        request_id: &ExternalRequestId,
        result: McpToolResult,
    ) -> Result<Value, McpServerError> {
        let pending = self
            .pending
            .get(request_id)
            .ok_or(McpServerError::UnknownToolCall)?;
        if !pending.authorized {
            return Err(McpServerError::ToolCallNotAuthorized);
        }
        validate_tool_result(&result)?;
        let pending = self
            .pending
            .remove(request_id)
            .ok_or(McpServerError::UnknownToolCall)?;
        tool_result(&pending.call.request_id, result)
    }

    pub fn fail_tool(
        &mut self,
        request_id: &ExternalRequestId,
        message: impl Into<String>,
    ) -> Result<Value, McpServerError> {
        let pending = self
            .pending
            .remove(request_id)
            .ok_or(McpServerError::UnknownToolCall)?;
        tool_result(
            &pending.call.request_id,
            McpToolResult {
                text: message.into(),
                structured_content: None,
                is_error: true,
            },
        )
    }

    fn initialize(
        &mut self,
        id: Option<ExternalRequestId>,
        params: Option<&Value>,
    ) -> Result<Option<McpServerAction>, McpServerError> {
        let Some(id) = id else {
            return Ok(None);
        };
        if self.lifecycle != McpLifecycle::New {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32600,
                "MCP session is already initialized",
            ))));
        }
        let requested_version = params
            .and_then(|params| params.get("protocolVersion"))
            .and_then(Value::as_str);
        if requested_version.is_none() {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "initialize requires protocolVersion",
            ))));
        }
        self.lifecycle = McpLifecycle::Initializing;
        Ok(Some(McpServerAction::Response(rpc_result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "hyper-term",
                    "title": "Hyper Term Agent Tools",
                    "version": env!("CARGO_PKG_VERSION"),
                    "description": "Brokered GenUI, Deno LSP, and diff proposals for Agent mode"
                },
                "instructions": "All tool calls are proposals. Hyper Term executes them only after Rust-owned policy and permission checks."
            }),
        ))))
    }

    fn mark_initialized(
        &mut self,
        id: Option<ExternalRequestId>,
    ) -> Result<Option<McpServerAction>, McpServerError> {
        if id.is_some() {
            return Ok(id.map(|id| {
                McpServerAction::Response(rpc_error(
                    id,
                    -32600,
                    "notifications/initialized must be a notification",
                ))
            }));
        }
        if self.lifecycle != McpLifecycle::Initializing {
            return Ok(None);
        }
        self.lifecycle = McpLifecycle::Ready;
        Ok(None)
    }

    fn list_tools(
        &self,
        id: Option<ExternalRequestId>,
        params: Option<&Value>,
    ) -> Result<Option<McpServerAction>, McpServerError> {
        let Some(id) = id else {
            return Ok(None);
        };
        if self.lifecycle != McpLifecycle::Ready {
            return Ok(Some(not_ready(id)));
        }
        if params
            .and_then(|params| params.get("cursor"))
            .is_some_and(|cursor| !cursor.is_null())
        {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "this bounded catalog has no additional pages",
            ))));
        }
        Ok(Some(McpServerAction::Response(rpc_result(
            id,
            json!({"tools": tool_definitions(&self.enabled_tools)}),
        ))))
    }

    fn call_tool(
        &mut self,
        id: Option<ExternalRequestId>,
        params: Option<&Value>,
    ) -> Result<Option<McpServerAction>, McpServerError> {
        let Some(id) = id else {
            return Ok(None);
        };
        if self.lifecycle != McpLifecycle::Ready {
            return Ok(Some(not_ready(id)));
        }
        let Some(params) = params else {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "tools/call requires params",
            ))));
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "tools/call requires a tool name",
            ))));
        };
        let Some(profile) = tool_profile(name) else {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "unknown Hyper Term tool",
            ))));
        };
        if !self.enabled_tools.contains(&profile.class) {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "tool is not enabled for this Agent session",
            ))));
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Ok(Some(McpServerAction::Response(rpc_error(
                id,
                -32602,
                "tool arguments must be an object",
            ))));
        }
        if let Err(error) = validate_arguments(profile.class, &arguments) {
            return Ok(Some(McpServerAction::Response(tool_result(
                &id,
                McpToolResult {
                    text: error,
                    structured_content: None,
                    is_error: true,
                },
            )?)));
        }
        if self.pending.len() == MAX_PENDING_TOOL_CALLS {
            return Err(McpServerError::PendingOverflow);
        }
        if self.pending.contains_key(&id) {
            return Err(McpServerError::DuplicateRequestId);
        }
        let payload = json!({"name": name, "arguments": arguments});
        let proposal = AgentEffectProposal {
            driver_id: self.driver_id,
            protocol: StructuredAgentProtocol::Mcp20251125,
            request_id: id.clone(),
            method: format!("tools/call:{name}"),
            kind: AgentEffectKind::Tool,
            summary: profile.summary.into(),
            required_capabilities: profile
                .required_capabilities
                .iter()
                .map(|value| (*value).into())
                .collect(),
            payload_sha256: sha256_value(&payload)?,
            thread_id: None,
            turn_id: None,
            item_id: None,
        };
        let call = McpToolCall {
            request_id: id.clone(),
            class: profile.class,
            name: name.into(),
            arguments,
            proposal,
        };
        self.pending.insert(
            id,
            PendingToolCall {
                call: call.clone(),
                authorized: false,
            },
        );
        Ok(Some(McpServerAction::ToolProposed(Box::new(call))))
    }

    fn empty_inventory(
        &self,
        id: Option<ExternalRequestId>,
        field: &'static str,
    ) -> Result<Option<McpServerAction>, McpServerError> {
        let Some(id) = id else {
            return Ok(None);
        };
        if self.lifecycle != McpLifecycle::Ready {
            return Ok(Some(not_ready(id)));
        }
        Ok(Some(McpServerAction::Response(rpc_result(
            id,
            json!({(field): []}),
        ))))
    }
}

struct ToolProfile {
    class: McpToolClass,
    summary: &'static str,
    required_capabilities: &'static [&'static str],
}

fn tool_profile(name: &str) -> Option<ToolProfile> {
    match name {
        "hyper_term.genui.compile" => Some(ToolProfile {
            class: McpToolClass::GenUiCompile,
            summary: "Compile a bounded GenUI artifact with the supervised Deno runtime",
            required_capabilities: &["artifact_build", "deno_runtime"],
        }),
        "hyper_term.lsp.query" => Some(ToolProfile {
            class: McpToolClass::DenoLspQuery,
            summary: "Query the supervised Deno LSP against a workspace snapshot",
            required_capabilities: &["workspace_snapshot_read", "deno_lsp"],
        }),
        "hyper_term.diff.review" => Some(ToolProfile {
            class: McpToolClass::DiffReview,
            summary: "Build a read-only diff review artifact from bounded input",
            required_capabilities: &["diff_review"],
        }),
        _ => None,
    }
}

fn tool_definitions(enabled: &[McpToolClass]) -> Vec<Value> {
    [
        (
            McpToolClass::GenUiCompile,
        json!({
            "name": "hyper_term.genui.compile",
            "title": "Compile GenUI",
            "description": "Propose compilation of bounded React/TypeScript source in Hyper Term's supervised Deno runtime. This tool cannot run shell commands or write workspace files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {"type": "string", "maxLength": 262144},
                    "entry": {
                        "type": "string",
                        "maxLength": 256,
                        "description": "Optional virtual TS/JS module path; defaults to /App.tsx"
                    }
                },
                "required": ["source"],
                "additionalProperties": false
            },
            "execution": {"taskSupport": "forbidden"}
        })),
        (
            McpToolClass::DenoLspQuery,
        json!({
            "name": "hyper_term.lsp.query",
            "title": "Query Deno LSP",
            "description": "Propose a bounded, allowlisted Deno LSP query against an authority-created workspace snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": {
                        "type": "string",
                        "enum": [
                            "textDocument/hover",
                            "textDocument/completion",
                            "textDocument/definition",
                            "textDocument/references",
                            "textDocument/documentSymbol",
                            "textDocument/formatting"
                        ]
                    },
                    "documentPath": {
                        "type": "string",
                        "maxLength": 4096,
                        "description": "UTF-8 path relative to the authority-created workspace snapshot"
                    },
                    "position": {
                        "type": "object",
                        "properties": {
                            "line": {"type": "integer", "minimum": 0},
                            "character": {"type": "integer", "minimum": 0}
                        },
                        "required": ["line", "character"],
                        "additionalProperties": false
                    },
                    "includeDeclaration": {"type": "boolean"}
                },
                "required": ["method", "documentPath"],
                "additionalProperties": false
            },
            "execution": {"taskSupport": "forbidden"}
        })),
        (
            McpToolClass::DiffReview,
        json!({
            "name": "hyper_term.diff.review",
            "title": "Build Diff Review",
            "description": "Propose a read-only diff artifact from two bounded text revisions. This tool does not read or modify files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "before": {"type": "string", "maxLength": 524288},
                    "after": {"type": "string", "maxLength": 524288},
                    "language": {"type": "string", "maxLength": 64}
                },
                "required": ["before", "after"],
                "additionalProperties": false
            },
            "execution": {"taskSupport": "forbidden"}
        })),
    ]
    .into_iter()
    .filter_map(|(class, definition)| enabled.contains(&class).then_some(definition))
    .collect()
}

fn validate_arguments(class: McpToolClass, arguments: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(arguments).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_MCP_ARGUMENT_BYTES {
        return Err(format!(
            "tool arguments exceed the {MAX_MCP_ARGUMENT_BYTES}-byte bound"
        ));
    }
    let object = arguments
        .as_object()
        .ok_or_else(|| "tool arguments must be an object".to_owned())?;
    match class {
        McpToolClass::GenUiCompile => {
            required_bounded_string(object, "source", 262_144)?;
            optional_bounded_string(object, "entry", 256)?;
            if let Some(entry) = object.get("entry").and_then(Value::as_str) {
                let normalized = entry.strip_prefix('/').unwrap_or(entry);
                if entry.contains('\\')
                    || entry.contains("..")
                    || normalized.is_empty()
                    || ![".tsx", ".ts", ".jsx", ".js"]
                        .iter()
                        .any(|extension| normalized.ends_with(extension))
                {
                    return Err("entry must be a bounded virtual TS/JS module path".into());
                }
            }
            reject_extra(object, &["source", "entry"])
        }
        McpToolClass::DenoLspQuery => {
            let method = required_bounded_string(object, "method", 64)?;
            const ALLOWED_METHODS: &[&str] = &[
                "textDocument/hover",
                "textDocument/completion",
                "textDocument/definition",
                "textDocument/references",
                "textDocument/documentSymbol",
                "textDocument/formatting",
            ];
            if !ALLOWED_METHODS.contains(&method) {
                return Err("LSP method is not in the read-only query allowlist".into());
            }
            required_bounded_string(object, "documentPath", 4096)?;
            let position_required = matches!(
                method,
                "textDocument/hover"
                    | "textDocument/completion"
                    | "textDocument/definition"
                    | "textDocument/references"
            );
            if position_required && !object.contains_key("position") {
                return Err(format!("{method} requires a position"));
            }
            if let Some(position) = object.get("position") {
                let position = position
                    .as_object()
                    .ok_or_else(|| "LSP position must be an object".to_owned())?;
                for key in ["line", "character"] {
                    if !position.get(key).is_some_and(Value::is_u64) {
                        return Err(format!("LSP position {key} must be a non-negative integer"));
                    }
                }
                reject_extra(position, &["line", "character"])?;
            }
            if object
                .get("includeDeclaration")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("includeDeclaration must be a boolean".into());
            }
            reject_extra(
                object,
                &["method", "documentPath", "position", "includeDeclaration"],
            )
        }
        McpToolClass::DiffReview => {
            required_bounded_string(object, "before", 524_288)?;
            required_bounded_string(object, "after", 524_288)?;
            optional_bounded_string(object, "language", 64)?;
            reject_extra(object, &["before", "after", "language"])
        }
    }
}

fn required_bounded_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<&'a str, String> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    if value.len() > maximum {
        return Err(format!("{key} exceeds the {maximum}-byte bound"));
    }
    Ok(value)
}

fn optional_bounded_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<(), String> {
    if let Some(value) = object.get(key) {
        let value = value
            .as_str()
            .ok_or_else(|| format!("{key} must be a string"))?;
        if value.len() > maximum {
            return Err(format!("{key} exceeds the {maximum}-byte bound"));
        }
    }
    Ok(())
}

fn reject_extra(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Err(format!("unknown tool argument: {key}"))
    } else {
        Ok(())
    }
}

fn validate_tool_result(result: &McpToolResult) -> Result<(), McpServerError> {
    if result.text.len() > MAX_MCP_RESULT_BYTES {
        return Err(McpServerError::ResultTooLarge);
    }
    if result
        .structured_content
        .as_ref()
        .is_some_and(|value| !value.is_object())
    {
        return Err(McpServerError::InvalidResult(
            "structured content must be a JSON object".into(),
        ));
    }
    if let Some(value) = &result.structured_content {
        ensure_size(value, MAX_MCP_RESULT_BYTES, "structured tool result")?;
    }
    Ok(())
}

fn tool_result(id: &ExternalRequestId, result: McpToolResult) -> Result<Value, McpServerError> {
    validate_tool_result(&result)?;
    let mut payload = json!({
        "content": [{"type": "text", "text": result.text}],
        "isError": result.is_error
    });
    if let Some(structured_content) = result.structured_content {
        payload["structuredContent"] = structured_content;
    }
    Ok(rpc_result(id.clone(), payload))
}

fn not_ready(id: ExternalRequestId) -> McpServerAction {
    McpServerAction::Response(rpc_error(
        id,
        -32002,
        "MCP session has not completed initialization",
    ))
}

fn rpc_id(value: Option<&Value>) -> Option<ExternalRequestId> {
    match value {
        Some(Value::String(value)) if value.len() <= 512 => {
            Some(ExternalRequestId::String(value.clone()))
        }
        Some(Value::Number(value)) if value.is_i64() => {
            Some(ExternalRequestId::Signed(value.as_i64().expect("checked")))
        }
        Some(Value::Number(value)) if value.is_u64() => Some(ExternalRequestId::Unsigned(
            value.as_u64().expect("checked"),
        )),
        _ => None,
    }
}

fn rpc_id_value(id: ExternalRequestId) -> Value {
    match id {
        ExternalRequestId::String(value) => Value::String(value),
        ExternalRequestId::Signed(value) => Value::from(value),
        ExternalRequestId::Unsigned(value) => Value::from(value),
    }
}

fn rpc_result(id: ExternalRequestId, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": rpc_id_value(id), "result": result})
}

fn rpc_error(id: ExternalRequestId, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": rpc_id_value(id),
        "error": {"code": code, "message": message}
    })
}

fn ensure_size(value: &Value, maximum: usize, label: &str) -> Result<(), McpServerError> {
    let size = serde_json::to_vec(value)?.len();
    if size > maximum {
        Err(McpServerError::FrameTooLarge {
            label: label.into(),
            size,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn sha256_value(value: &Value) -> Result<String, McpServerError> {
    let digest = Sha256::digest(serde_json::to_vec(value)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Error)]
pub enum McpServerError {
    #[error("MCP JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{label} is {size} bytes, above the {maximum}-byte bound")]
    FrameTooLarge {
        label: String,
        size: usize,
        maximum: usize,
    },
    #[error("MCP pending tool calls exceeded their bound")]
    PendingOverflow,
    #[error("MCP client reused a pending request ID")]
    DuplicateRequestId,
    #[error("MCP tool call is no longer pending")]
    UnknownToolCall,
    #[error("MCP tool call has already been authorized")]
    AlreadyAuthorized,
    #[error("MCP tool call has not been authorized")]
    ToolCallNotAuthorized,
    #[error("invalid MCP authorization: {0}")]
    InvalidAuthorization(String),
    #[error("MCP tool result exceeds its size bound")]
    ResultTooLarge,
    #[error("invalid MCP tool result: {0}")]
    InvalidResult(String),
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::{OperationId, PermissionDecision};

    use super::*;

    fn initialized_server() -> McpAgentServer {
        let mut server = McpAgentServer::new(Uuid::nil());
        let initialize = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"}
                }
            }))
            .unwrap();
        assert!(matches!(initialize, Some(McpServerAction::Response(_))));
        assert_eq!(
            server
                .receive(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                }))
                .unwrap(),
            None
        );
        server
    }

    #[test]
    fn initialization_advertises_only_brokered_agent_tools() {
        let mut server = initialized_server();
        let Some(McpServerAction::Response(response)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": "list-1",
                "method": "tools/list",
                "params": {}
            }))
            .unwrap()
        else {
            panic!("expected tools/list response");
        };
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert!(tools.iter().all(|tool| {
            !tool["name"]
                .as_str()
                .is_some_and(|name| name.contains("shell") || name.contains("file"))
        }));
    }

    #[test]
    fn tool_call_is_a_proposal_until_broker_authorizes_it() {
        let mut server = initialized_server();
        let Some(McpServerAction::ToolProposed(call)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "hyper_term.genui.compile",
                    "arguments": {"source": "export default () => <p>Hello</p>"}
                }
            }))
            .unwrap()
        else {
            panic!("expected tool proposal");
        };
        assert_eq!(call.class, McpToolClass::GenUiCompile);
        assert_eq!(call.proposal.protocol, StructuredAgentProtocol::Mcp20251125);
        assert_eq!(call.proposal.kind, AgentEffectKind::Tool);
        assert_eq!(call.proposal.payload_sha256.len(), 64);
        assert_eq!(server.pending_calls(), vec![(*call).clone()]);
        assert!(matches!(
            server.complete_tool(
                &call.request_id,
                McpToolResult::success("not allowed yet", None)
            ),
            Err(McpServerError::ToolCallNotAuthorized)
        ));

        let outcome = server
            .authorize_tool(
                &call.request_id,
                AgentEffectAuthorization {
                    operation_id: OperationId::new(),
                    operation_revision: 2,
                    proposal_sha256: call.proposal.payload_sha256.clone(),
                    decision: PermissionDecision::AllowOnce,
                },
            )
            .unwrap();
        assert!(matches!(outcome, McpAuthorizationOutcome::Authorized(_)));
        let response = server
            .complete_tool(
                &call.request_id,
                McpToolResult::success(
                    "artifact compiled",
                    Some(json!({"artifactId": "artifact-1"})),
                ),
            )
            .unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["artifactId"],
            "artifact-1"
        );
        assert!(server.pending_calls().is_empty());
    }

    #[test]
    fn rejected_tool_call_returns_a_tool_error_without_execution() {
        let mut server = initialized_server();
        let Some(McpServerAction::ToolProposed(call)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": "diff-1",
                "method": "tools/call",
                "params": {
                    "name": "hyper_term.diff.review",
                    "arguments": {"before": "old", "after": "new"}
                }
            }))
            .unwrap()
        else {
            panic!("expected tool proposal");
        };
        let McpAuthorizationOutcome::Rejected(response) = server
            .authorize_tool(
                &call.request_id,
                AgentEffectAuthorization {
                    operation_id: OperationId::new(),
                    operation_revision: 1,
                    proposal_sha256: call.proposal.payload_sha256,
                    decision: PermissionDecision::RejectOnce,
                },
            )
            .unwrap()
        else {
            panic!("expected rejection response");
        };
        assert_eq!(response["result"]["isError"], true);
        assert!(server.pending_calls().is_empty());
    }

    #[test]
    fn lsp_tool_rejects_methods_outside_the_read_only_allowlist() {
        let mut server = initialized_server();
        let Some(McpServerAction::Response(response)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "hyper_term.lsp.query",
                    "arguments": {
                        "method": "workspace/executeCommand",
                        "documentPath": "main.ts"
                    }
                }
            }))
            .unwrap()
        else {
            panic!("expected validation response");
        };
        assert_eq!(response["result"]["isError"], true);
        assert!(server.pending_calls().is_empty());
    }

    #[test]
    fn tools_are_unavailable_before_initialized_notification() {
        let mut server = McpAgentServer::new(Uuid::nil());
        let Some(McpServerAction::Response(response)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }))
            .unwrap()
        else {
            panic!("expected not-ready response");
        };
        assert_eq!(response["error"]["code"], -32002);
    }

    #[test]
    fn broker_failure_finishes_a_pending_call_without_authorization() {
        let mut server = initialized_server();
        let Some(McpServerAction::ToolProposed(call)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "tools/call",
                "params": {
                    "name": "hyper_term.diff.review",
                    "arguments": {"before": "a", "after": "b"}
                }
            }))
            .unwrap()
        else {
            panic!("expected proposal");
        };
        let response = server
            .fail_tool(&call.request_id, "permission broker unavailable")
            .unwrap();
        assert_eq!(response["id"], 11);
        assert_eq!(response["result"]["isError"], true);
        assert!(server.pending_calls().is_empty());
    }

    #[test]
    fn codex_inventory_probes_receive_empty_resource_lists() {
        let mut server = initialized_server();
        for (id, method, field) in [
            (20, "resources/list", "resources"),
            (21, "resources/templates/list", "resourceTemplates"),
        ] {
            let Some(McpServerAction::Response(response)) = server
                .receive(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": {}
                }))
                .unwrap()
            else {
                panic!("expected resource inventory response");
            };
            assert_eq!(response["result"][field], json!([]));
        }
    }

    #[test]
    fn tool_catalog_reflects_the_executors_available_to_this_session() {
        let mut server = McpAgentServer::with_tools(Uuid::nil(), [McpToolClass::DiffReview]);
        server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {"protocolVersion": MCP_PROTOCOL_VERSION}
            }))
            .unwrap();
        server
            .receive(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .unwrap();
        let Some(McpServerAction::Response(list)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }))
            .unwrap()
        else {
            panic!("expected tool list");
        };
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 1);
        assert_eq!(list["result"]["tools"][0]["name"], "hyper_term.diff.review");
        let Some(McpServerAction::Response(disabled)) = server
            .receive(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "hyper_term.lsp.query",
                    "arguments": {"method": "textDocument/hover", "documentPath": "main.ts"}
                }
            }))
            .unwrap()
        else {
            panic!("expected disabled tool response");
        };
        assert_eq!(disabled["error"]["code"], -32602);
    }
}
