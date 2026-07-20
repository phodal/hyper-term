use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ContextDigest, McpArgumentsDigest, McpRuntimeIdentityDigest, SandboxProfileDigest};

pub const LOCAL_MCP_LAUNCH_SCHEMA_VERSION: u16 = 1;

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
