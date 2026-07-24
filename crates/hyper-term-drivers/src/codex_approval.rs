use hyper_term_protocol::PermissionDecision;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    CodexAdapterError, PendingCodexApproval, bounded, optional_bounded_string, required_string,
    sha256_value,
};
use crate::{AgentEffectKind, AgentEffectProposal, ExternalRequestId, StructuredAgentProtocol};

pub(super) fn normalize_effect(
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
        "item/permissions/requestApproval" => {
            let permissions = normalize_permission_profile(params.get("permissions"))?;
            let mut capabilities = vec!["opaque_effect".into()];
            if permissions
                .pointer("/network/enabled")
                .and_then(Value::as_bool)
                == Some(true)
            {
                capabilities.push("network".into());
            }
            if permission_profile_requests_write(&permissions) {
                capabilities.push("workspace_write".into());
            } else if !permissions["fileSystem"].is_null() {
                capabilities.push("filesystem_read".into());
            }
            (
                AgentEffectKind::Opaque,
                reason
                    .unwrap_or("Codex requested additional permissions")
                    .to_owned(),
                capabilities,
            )
        }
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

pub(super) fn normalize_permission_profile(
    value: Option<&Value>,
) -> Result<Value, CodexAdapterError> {
    let value = value.ok_or_else(|| {
        CodexAdapterError::InvalidMessage("permission request omitted its profile".into())
    })?;
    let profile = value.as_object().ok_or_else(|| {
        CodexAdapterError::InvalidMessage("permission profile must be an object".into())
    })?;
    if profile
        .keys()
        .any(|key| key != "network" && key != "fileSystem")
    {
        return Err(CodexAdapterError::InvalidMessage(
            "permission profile contains an unsupported field".into(),
        ));
    }
    if serde_json::to_vec(value)?.len() > 256 * 1024 {
        return Err(CodexAdapterError::InvalidMessage(
            "permission profile exceeds 256 KiB".into(),
        ));
    }
    Ok(value.clone())
}

fn permission_profile_requests_write(permissions: &Value) -> bool {
    let Some(file_system) = permissions.get("fileSystem").and_then(Value::as_object) else {
        return false;
    };
    if file_system
        .get("write")
        .and_then(Value::as_array)
        .is_some_and(|paths| !paths.is_empty())
    {
        return true;
    }
    file_system
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.get("access").and_then(Value::as_str) == Some("write"))
        })
}

pub(super) fn brokered_mcp_elicitation_response(
    params: &Value,
    trusted_brokered_mcp_server: bool,
) -> Result<Value, CodexAdapterError> {
    let server_name = required_string(params, "serverName")?;
    let mode = required_string(params, "mode")?;
    let is_tool_approval = params
        .pointer("/_meta/codex_approval_kind")
        .and_then(Value::as_str)
        == Some("mcp_tool_call");
    let is_form = matches!(mode.as_str(), "form" | "openai/form");

    // Codex asks its host to approve an MCP call before invoking the server.
    // Hyper Term's private, digest-pinned MCP process can only submit a bounded
    // proposal to the Rust authority, which creates the one user-facing
    // approval and owns execution. Let that proposal reach the broker without
    // adding a duplicate host-level approval. Every other elicitation remains
    // closed by default.
    if trusted_brokered_mcp_server && server_name == "hyper_term" && is_tool_approval && is_form {
        Ok(json!({"action": "accept", "content": null, "_meta": null}))
    } else {
        Ok(json!({"action": "cancel", "content": null, "_meta": null}))
    }
}

pub(super) fn codex_approval_result(
    pending: &PendingCodexApproval,
    authorization: PermissionDecision,
    decision: &str,
) -> Value {
    match &pending.requested_permissions {
        Some(permissions) if authorization == PermissionDecision::AllowOnce => json!({
            "permissions": permissions,
            "scope": "turn",
            "strictAutoReview": false
        }),
        Some(_) => json!({
            "permissions": {},
            "scope": "turn",
            "strictAutoReview": true
        }),
        None => json!({"decision": decision}),
    }
}
