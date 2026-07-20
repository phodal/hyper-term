use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    ContextDigest, McpArgumentsDigest, McpCapabilitiesDigest, McpCatalogDigest,
    McpRuntimeIdentityDigest, McpToolContractDigest, McpToolResultDigest, SandboxProfileDigest,
};

pub const LOCAL_MCP_LAUNCH_SCHEMA_VERSION: u16 = 1;
pub const LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION: u16 = 1;

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
