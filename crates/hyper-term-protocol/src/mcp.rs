use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ContextDigest, McpArgumentsDigest, McpCapabilitiesDigest, McpCatalogDigest,
    McpRuntimeIdentityDigest, McpToolContractDigest, McpToolResultDigest, SandboxProfileDigest,
};

pub const LOCAL_MCP_LAUNCH_SCHEMA_VERSION: u16 = 1;
pub const LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION: u16 = 1;
pub const BROKERED_MCP_TOOL_CALL_SCHEMA_VERSION: u16 = 1;

/// Stable JSON encoding used for brokered MCP argument and proposal digests.
/// Object keys are sorted recursively so a parser or adapter cannot change an
/// authorization identity merely by preserving a different insertion order.
pub fn canonical_mcp_json_bytes(value: &Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&canonical_mcp_json_value(value))
}

fn canonical_mcp_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_mcp_json_value).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::with_capacity(object.len());
            for key in keys {
                canonical.insert(key.clone(), canonical_mcp_json_value(&object[key]));
            }
            Value::Object(canonical)
        }
        scalar => scalar.clone(),
    }
}

/// Live, operation-bound result returned by the Rust authority after it runs a
/// bundled MCP tool outside the Agent process tree. Only the final receipt is
/// journaled; structured content remains on the bounded control connection.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BrokeredMcpToolExecution {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    pub is_error: bool,
    pub outcome: crate::OperationOutcome,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalMcpServerLifecycle {
    OneTask,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalMcpCredentialScope {
    ServerLifetime,
    SplitPrivilege,
}

/// Redacted, durable identity for one reviewed local stdio MCP launch.
///
/// The executable arguments and environment stay inside the daemon. Their
/// digests bind the approval without persisting possible credential material.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalMcpServerLaunch {
    pub schema_version: u16,
    pub server_id: String,
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub arguments_digest: McpArgumentsDigest,
    pub argument_count: u16,
    pub working_directory: PathBuf,
    pub context_digest: ContextDigest,
    pub sandbox_profile_digest: SandboxProfileDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots_snapshot_sha256: Option<String>,
    pub lifecycle: LocalMcpServerLifecycle,
    pub credential_scope: LocalMcpCredentialScope,
    pub runtime_identity_digest: McpRuntimeIdentityDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalMcpToolContractReceipt {
    pub name: String,
    pub input_schema_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_sha256: Option<String>,
    pub contract_digest: McpToolContractDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalMcpServerRuntimeReceipt {
    pub schema_version: u16,
    pub launch: LocalMcpServerLaunch,
    pub negotiated_protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub enforced_sandbox_profile_digest: SandboxProfileDigest,
    pub capabilities_digest: McpCapabilitiesDigest,
    pub catalog_digest: McpCatalogDigest,
    pub runtime_identity_digest: McpRuntimeIdentityDigest,
    #[serde(default)]
    pub tools: Vec<LocalMcpToolContractReceipt>,
    pub credential_scope: LocalMcpCredentialScope,
    pub per_call_isolation: bool,
}

/// Redacted identity for one exact invocation against a negotiated local MCP
/// runtime. Argument values stay in live execution memory and only their digest
/// enters the operation journal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalMcpToolCall {
    pub schema_version: u16,
    pub server_id: String,
    pub runtime_identity_digest: McpRuntimeIdentityDigest,
    pub catalog_digest: McpCatalogDigest,
    pub tool_name: String,
    pub tool_contract_digest: McpToolContractDigest,
    pub arguments_digest: McpArgumentsDigest,
}

/// Durable, redacted evidence returned after an authorized MCP tool call.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalMcpToolCallReceipt {
    pub schema_version: u16,
    pub call: LocalMcpToolCall,
    pub succeeded: bool,
    pub result_digest: McpToolResultDigest,
    pub content_count: u16,
    pub has_structured_content: bool,
}

/// Rust-derived, bounded review identity for exact arguments proposed through
/// Hyper Term's built-in MCP server.
///
/// The complete arguments stay on the live capability connection so source or
/// diff content is not copied into the operation journal. Rust persists a
/// labelled preview plus exact byte count and digests, and later recomputes the
/// same digests over the live execution request before dispatch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrokeredMcpToolCall {
    pub schema_version: u16,
    pub server_id: String,
    pub tool_name: String,
    pub canonical_arguments_preview: String,
    pub arguments_bytes: u32,
    pub arguments_truncated: bool,
    pub arguments_digest: McpArgumentsDigest,
    pub proposal_digest: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brokered_mcp_json_is_canonical_across_nested_object_order() {
        let first = serde_json::json!({
            "z": {"second": 2, "first": 1},
            "a": true
        });
        let second = serde_json::json!({
            "a": true,
            "z": {"first": 1, "second": 2}
        });
        assert_eq!(
            canonical_mcp_json_bytes(&first).unwrap(),
            canonical_mcp_json_bytes(&second).unwrap()
        );
        assert_eq!(
            canonical_mcp_json_bytes(&first).unwrap(),
            br#"{"a":true,"z":{"first":1,"second":2}}"#
        );
    }
}
