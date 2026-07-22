use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    ActionDigest, ApprovalDetailDigest, McpArgumentsDigest, McpCatalogDigest,
    McpRuntimeIdentityDigest, McpToolContractDigest, OperationId, RiskClass, SandboxProfileDigest,
};

pub const APPROVAL_DETAIL_SCHEMA_VERSION: u16 = 1;

/// Rust-authenticated, bounded evidence shown before a human authorizes an effect.
///
/// Secret environment values never enter this projection. Command arguments are
/// preserved as distinct items, with likely credential values replaced by a
/// stable redaction marker before the detail reaches a renderer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalDetail {
    pub schema_version: u16,
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub action_digest: ActionDigest,
    pub action: ApprovalActionDetail,
    pub risk: RiskClass,
    #[serde(default)]
    pub effective_capabilities: Vec<String>,
    pub opaque_effect: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalActionDetail {
    Shell {
        program: String,
        #[serde(default)]
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        #[serde(default)]
        environment_keys: Vec<String>,
    },
    McpServerLaunch {
        server_id: String,
        executable: PathBuf,
        executable_sha256: String,
        argument_count: u16,
        arguments_digest: McpArgumentsDigest,
        working_directory: PathBuf,
        sandbox_profile_digest: SandboxProfileDigest,
    },
    McpTool {
        server_id: String,
        tool_name: String,
        runtime_identity_digest: McpRuntimeIdentityDigest,
        catalog_digest: McpCatalogDigest,
        tool_contract_digest: McpToolContractDigest,
        arguments_digest: McpArgumentsDigest,
    },
    BrokeredMcpTool {
        server_id: String,
        tool_name: String,
        canonical_arguments_preview: String,
        arguments_bytes: u32,
        arguments_truncated: bool,
        arguments_digest: McpArgumentsDigest,
        proposal_digest: String,
    },
    Opaque {
        kind: String,
        payload_digest: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BoundApprovalDetail {
    pub detail: ApprovalDetail,
    pub detail_digest: ApprovalDetailDigest,
}
