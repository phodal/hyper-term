use super::*;

pub(super) fn validate_local_mcp_tool_call(call: &LocalMcpToolCall) -> Result<(), DaemonError> {
    if call.schema_version != hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION
        || call.server_id.is_empty()
        || call.server_id.len() > 64
        || !call
            .server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !valid_mcp_receipt_text(&call.tool_name, 256)
    {
        return Err(DaemonError::InvalidLocalMcpToolCall);
    }
    Ok(())
}

pub(super) fn validate_local_mcp_tool_call_receipt(
    receipt: &LocalMcpToolCallReceipt,
) -> Result<(), DaemonError> {
    validate_local_mcp_tool_call(&receipt.call)
        .map_err(|_| DaemonError::InvalidLocalMcpToolCallReceipt)?;
    if receipt.schema_version != hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION {
        return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
    }
    let encoded =
        serde_json::to_vec(receipt).map_err(|_| DaemonError::InvalidLocalMcpToolCallReceipt)?;
    if encoded.len() > 16 * 1024 {
        return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
    }
    Ok(())
}

pub(super) fn validate_mcp_server_launch(
    launch: &hyper_term_protocol::LocalMcpServerLaunch,
) -> Result<(), DaemonError> {
    if launch.schema_version != hyper_term_protocol::LOCAL_MCP_LAUNCH_SCHEMA_VERSION
        || launch.server_id.is_empty()
        || launch.server_id.len() > 64
        || !launch
            .server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || launch.argument_count > 64
        || !launch.executable.is_absolute()
        || !launch.working_directory.is_absolute()
        || !is_sha256(&launch.executable_sha256)
        || launch
            .roots_snapshot_sha256
            .as_deref()
            .is_some_and(|digest| !is_sha256(digest))
    {
        return Err(DaemonError::InvalidMcpServerLaunch);
    }
    let executable =
        fs::canonicalize(&launch.executable).map_err(|_| DaemonError::InvalidMcpServerLaunch)?;
    let working_directory = fs::canonicalize(&launch.working_directory)
        .map_err(|_| DaemonError::InvalidMcpServerLaunch)?;
    if executable != launch.executable
        || !executable.is_file()
        || working_directory != launch.working_directory
        || !working_directory.is_dir()
    {
        return Err(DaemonError::InvalidMcpServerLaunch);
    }
    Ok(())
}

#[derive(Serialize)]
struct McpToolContractIdentity<'a> {
    planned_runtime_identity: &'a str,
    name: &'a str,
    input_schema_sha256: &'a str,
    output_schema_sha256: Option<&'a str>,
}

#[derive(Serialize)]
struct NegotiatedMcpRuntimeIdentity<'a> {
    planned_runtime_identity: &'a str,
    negotiated_protocol_version: &'a str,
    server_name: &'a str,
    server_version: &'a str,
    enforced_sandbox_profile_digest: &'a str,
    capabilities_digest: &'a str,
    catalog_digest: &'a str,
}

pub(super) fn validate_local_mcp_runtime_receipt(
    receipt: &LocalMcpServerRuntimeReceipt,
) -> Result<(), DaemonError> {
    validate_mcp_server_launch(&receipt.launch)
        .map_err(|_| DaemonError::InvalidLocalMcpRuntimeReceipt)?;
    if receipt.schema_version != hyper_term_protocol::LOCAL_MCP_LAUNCH_SCHEMA_VERSION
        || !valid_mcp_receipt_text(&receipt.negotiated_protocol_version, 128)
        || !valid_mcp_receipt_text(&receipt.server_name, 256)
        || !valid_mcp_receipt_text(&receipt.server_version, 128)
        || receipt.enforced_sandbox_profile_digest != receipt.launch.sandbox_profile_digest
        || receipt.credential_scope != receipt.launch.credential_scope
        || receipt.per_call_isolation
        || receipt.tools.len() > 256
    {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }

    let mut previous_name: Option<&str> = None;
    for tool in &receipt.tools {
        if !valid_mcp_receipt_text(&tool.name, 256)
            || previous_name.is_some_and(|previous| previous >= tool.name.as_str())
            || !is_sha256(&tool.input_schema_sha256)
            || tool
                .output_schema_sha256
                .as_deref()
                .is_some_and(|digest| !is_sha256(digest))
        {
            return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
        }
        let expected_contract = mcp_receipt_sha256(&McpToolContractIdentity {
            planned_runtime_identity: receipt.launch.runtime_identity_digest.as_str(),
            name: &tool.name,
            input_schema_sha256: &tool.input_schema_sha256,
            output_schema_sha256: tool.output_schema_sha256.as_deref(),
        })?;
        if tool.contract_digest.as_str() != expected_contract {
            return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
        }
        previous_name = Some(&tool.name);
    }

    let expected_catalog = mcp_receipt_sha256(&receipt.tools)?;
    if receipt.catalog_digest.as_str() != expected_catalog {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }
    let expected_runtime = mcp_receipt_sha256(&NegotiatedMcpRuntimeIdentity {
        planned_runtime_identity: receipt.launch.runtime_identity_digest.as_str(),
        negotiated_protocol_version: &receipt.negotiated_protocol_version,
        server_name: &receipt.server_name,
        server_version: &receipt.server_version,
        enforced_sandbox_profile_digest: receipt.enforced_sandbox_profile_digest.as_str(),
        capabilities_digest: receipt.capabilities_digest.as_str(),
        catalog_digest: receipt.catalog_digest.as_str(),
    })?;
    if receipt.runtime_identity_digest.as_str() != expected_runtime {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }
    let encoded =
        serde_json::to_vec(receipt).map_err(|_| DaemonError::InvalidLocalMcpRuntimeReceipt)?;
    if encoded.len() > 512 * 1024 {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }
    Ok(())
}

fn valid_mcp_receipt_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn mcp_receipt_sha256(value: &impl Serialize) -> Result<String, DaemonError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| DaemonError::InvalidLocalMcpRuntimeReceipt)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) fn validate_agent_tool_call(
    call: &hyper_term_protocol::AgentToolCall,
) -> Result<(), DaemonError> {
    if call.tool_call_id.is_empty()
        || call.tool_call_id.len() > 4096
        || call.title.is_empty()
        || call.title.len() > 16 * 1024
        || call.content.len() > 128
        || call.locations.len() > 128
    {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent tool call identity, title, or collection count is invalid".into(),
        ));
    }
    let encoded = serde_json::to_vec(call)
        .map_err(|error| DaemonError::InvalidAgentProjection(error.to_string()))?;
    if encoded.len() > 512 * 1024 {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent tool call exceeds the 512 KiB journal bound".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_agent_execution_context(
    provider_id: &str,
    protocol: &str,
    thread_id: &str,
    receipts: &[ContextReceipt],
) -> Result<(), DaemonError> {
    if provider_id.is_empty()
        || provider_id.len() > 64
        || !provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || protocol.is_empty()
        || protocol.len() > 64
        || !protocol
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || thread_id.is_empty()
        || thread_id.len() > 4096
        || thread_id.chars().any(char::is_control)
        || receipts.is_empty()
        || receipts.len() > 4
    {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent execution-context identity or receipt count is invalid".into(),
        ));
    }
    let mut context_ids = HashSet::new();
    for receipt in receipts {
        if receipt.schema_version != EXECUTION_CONTEXT_SCHEMA_VERSION
            || receipt.context_id.is_empty()
            || receipt.context_id.len() > 128
            || !context_ids.insert(receipt.context_id.as_str())
            || receipt.bindings.len() > 128
            || receipt.credential_bindings.len() > 32
            || receipt.credential_bindings.iter().any(|credential| {
                credential.binding_id.is_empty()
                    || credential.binding_id.len() > 128
                    || credential.reference.provider_id.is_empty()
                    || credential.reference.provider_id.len() > 128
                    || credential.reference.secret_id.is_empty()
                    || credential.reference.secret_id.len() > 256
                    || credential.target_name.is_empty()
                    || credential.target_name.len() > 128
                    || credential.audience.is_empty()
                    || credential.audience.len() > 2048
                    || credential.audience.chars().any(char::is_control)
            })
        {
            return Err(DaemonError::InvalidAgentProjection(
                "Agent execution-context receipt is invalid or unbounded".into(),
            ));
        }
    }
    let encoded = serde_json::to_vec(receipts)
        .map_err(|error| DaemonError::InvalidAgentProjection(error.to_string()))?;
    if encoded.len() > 256 * 1024 {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent execution-context receipts exceed the 256 KiB journal bound".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_operation_scope(
    record: &OperationRecord,
    task_id: TaskId,
    expected_revision: u64,
) -> Result<(), DaemonError> {
    if record.task_id != task_id {
        return Err(DaemonError::OperationTaskMismatch {
            expected: record.task_id,
            actual: task_id,
        });
    }
    if record.revision != expected_revision {
        return Err(DaemonError::StaleOperationRevision {
            expected: record.revision,
            actual: expected_revision,
        });
    }
    Ok(())
}

pub(super) fn bounded_nonempty(
    value: String,
    maximum: usize,
    label: &'static str,
) -> Result<String, DaemonError> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > maximum {
        Err(DaemonError::InvalidBoundedText { label, maximum })
    } else {
        Ok(value)
    }
}

pub(super) fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
