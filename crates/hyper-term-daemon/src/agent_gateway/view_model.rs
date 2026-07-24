//! HTTP request and response contracts for the Agent gateway.
//!
//! These values are bounded projections and inputs only. Runtime ownership,
//! process lifecycle, permissions, and durable state remain in the parent
//! gateway and Rust authority layers.

use std::collections::BTreeMap;

use hyper_term_drivers::{AgentSessionCapabilities, AgentSessionConfigValue, AgentThreadGoal};
use hyper_term_protocol::{
    AcceptedGenUiArtifact, ApprovalDetailDigest, ArtifactId, BlockDocument, EventEnvelope,
    LocalMcpServerRuntimeReceipt, LocalMcpToolCallReceipt, OperationId, OperationState,
    PermissionDecision, TaskId,
};
use hyper_term_sandbox::{IsolatedChange, IsolatedTaskTermination};
use serde::{Deserialize, Serialize};

use crate::local_mcp_runtime::RegisteredLocalMcpServer;
use crate::workspace_diff::WorkspaceDiffHunk;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentStatus {
    Ready,
    Running,
    Cancelling,
    Completed,
    WaitingApproval,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentFailureStage {
    Provider,
    Mcp,
    Approval,
    Compile,
    Artifact,
    Turn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentFailureKind {
    UserRejected,
    UserCancelled,
    PolicyRejected,
    RuntimeFailure,
    InvalidResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentFailureRecovery {
    RetrySameTurn,
    RestartProvider,
    ReviewApproval,
    RefreshProvider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentFailureAuthority {
    ProposalOnly,
    RustPermissionBroker,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct AgentFailure {
    pub(super) stage: AgentFailureStage,
    pub(super) kind: AgentFailureKind,
    pub(super) recovery: AgentFailureRecovery,
    pub(super) authority: AgentFailureAuthority,
    pub(super) retryable: bool,
    pub(super) operation_id: Option<OperationId>,
    pub(super) message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(super) struct AgentBuildIdentity {
    pub(super) version: &'static str,
    pub(super) source_commit: &'static str,
}

pub(super) fn agent_build_identity() -> AgentBuildIdentity {
    AgentBuildIdentity {
        version: env!("CARGO_PKG_VERSION"),
        source_commit: option_env!("HYPER_TERM_SOURCE_COMMIT").unwrap_or("unknown"),
    }
}

#[derive(Deserialize)]
pub(super) struct AgentSessionQuery {
    pub(super) token: Option<String>,
    pub(super) session_id: Option<u16>,
    pub(super) provider: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AgentSessionResponse {
    pub(super) session_id: u16,
    pub(super) provider: String,
    pub(super) protocol: String,
    pub(super) status: &'static str,
    pub(super) task_id: TaskId,
    pub(super) thread_id: String,
    pub(super) history_restored: bool,
}

#[derive(Serialize)]
pub(super) struct AgentSnapshotResponse {
    pub(super) session_id: u16,
    pub(super) task_id: TaskId,
    pub(super) build: AgentBuildIdentity,
    pub(super) status: AgentStatus,
    pub(super) turn_id: Option<String>,
    pub(super) error: Option<String>,
    pub(super) failure: Option<AgentFailure>,
    pub(super) history_restored: bool,
    pub(super) pending_operation_id: Option<OperationId>,
    pub(super) capabilities: AgentSessionCapabilities,
    pub(super) goal: Option<AgentThreadGoal>,
    pub(super) context: Option<EventEnvelope>,
    pub(super) document: BlockDocument,
}

/// Low-bandwidth desktop attention projection. This intentionally excludes
/// transcript content, errors, capabilities, and operation payloads: the
/// Native shell only needs authenticated lifecycle identity for background
/// tabs, while full state stays on the per-session snapshot/stream routes.
#[derive(Serialize)]
pub(super) struct AgentAttentionResponse {
    pub(super) sessions: Vec<AgentAttentionSession>,
}

#[derive(Serialize)]
pub(super) struct AgentAttentionSession {
    pub(super) session_id: u16,
    pub(super) provider: String,
    pub(super) status: AgentStatus,
    pub(super) document_revision: u64,
}

#[derive(Deserialize)]
pub(super) struct AgentConfigRequest {
    pub(super) config_id: String,
    pub(super) value: AgentSessionConfigValue,
}

#[derive(Serialize)]
pub(super) struct AgentCapabilitiesResponse {
    pub(super) session_id: u16,
    pub(super) capabilities: AgentSessionCapabilities,
}

#[derive(Serialize)]
pub(super) struct AgentTurnResponse {
    pub(super) session_id: u16,
    pub(super) status: AgentStatus,
}

#[derive(Serialize)]
pub(super) struct AgentArtifactSourceResponse {
    pub(super) artifact_id: ArtifactId,
    pub(super) source_revision: u64,
    pub(super) entrypoint: String,
    pub(super) files: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub(super) struct AgentArtifactHistoryResponse {
    pub(super) active_artifact_id: ArtifactId,
    pub(super) entries: Vec<AgentArtifactHistoryEntry>,
}

#[derive(Serialize)]
pub(super) struct AgentArtifactHistoryEntry {
    pub(super) event_sequence: u64,
    pub(super) recorded_at_ms: u64,
    pub(super) operation_id: Option<OperationId>,
    pub(super) artifact: AcceptedGenUiArtifact,
}

#[derive(Deserialize)]
pub(super) struct AgentArtifactDraftRequest {
    pub(super) base_source_revision: u64,
    pub(super) entrypoint: String,
    pub(super) files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub(super) struct AgentArtifactDraftStatusQuery {
    pub(super) token: Option<String>,
    pub(super) session_id: Option<u16>,
    pub(super) operation_id: Option<OperationId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ArtifactDraftStatus {
    WaitingApproval,
    Compiling,
    Accepted,
    Rejected,
    Failed,
}

#[derive(Serialize)]
pub(super) struct AgentArtifactDraftResponse {
    pub(super) operation_id: OperationId,
    pub(super) operation_revision: u64,
    pub(super) status: ArtifactDraftStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) artifact: Option<AcceptedGenUiArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AgentWorkspaceApplyRequest {
    pub(super) artifact_source_revision: u64,
    #[serde(default)]
    pub(super) review_digest: Option<String>,
    #[serde(default)]
    pub(super) source_path: Option<String>,
    #[serde(default)]
    pub(super) target_path: Option<String>,
    #[serde(default)]
    pub(super) mappings: Vec<AgentWorkspaceApplyMapping>,
}

#[derive(Deserialize)]
pub(super) struct AgentWorkspaceApplyMapping {
    pub(super) source_path: String,
    pub(super) target_path: String,
    #[serde(default)]
    pub(super) hunk_ids: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct AgentWorkspacePreviewResponse {
    pub(super) artifact_source_revision: u64,
    pub(super) review_digest: String,
    pub(super) changes: Vec<AgentWorkspacePreviewChangeResponse>,
}

#[derive(Serialize)]
pub(super) struct AgentWorkspacePreviewChangeResponse {
    pub(super) source_path: String,
    pub(super) target_path: String,
    pub(super) base_digest: Option<String>,
    pub(super) artifact_digest: String,
    pub(super) before: String,
    pub(super) artifact_after: String,
    pub(super) hunks: Vec<WorkspaceDiffHunk>,
}

#[derive(Deserialize)]
pub(super) struct AgentWorkspaceApplyStatusQuery {
    pub(super) token: Option<String>,
    pub(super) session_id: Option<u16>,
    pub(super) operation_id: Option<OperationId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkspaceApplyStatus {
    WaitingApproval,
    Applying,
    Applied,
    Rejected,
    Failed,
    UnknownExecution,
}

#[derive(Serialize)]
pub(super) struct AgentWorkspaceApplyResponse {
    pub(super) operation_id: OperationId,
    pub(super) operation_revision: u64,
    pub(super) status: WorkspaceApplyStatus,
    pub(super) artifact_source_revision: u64,
    pub(super) source_path: String,
    pub(super) target_path: String,
    pub(super) base_digest: Option<String>,
    pub(super) proposed_digest: String,
    pub(super) before: String,
    pub(super) after: String,
    pub(super) transaction_digest: String,
    pub(super) changes: Vec<AgentWorkspaceApplyChangeResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AgentWorkspaceApplyChangeResponse {
    pub(super) source_path: String,
    pub(super) target_path: String,
    pub(super) base_digest: Option<String>,
    pub(super) proposed_digest: String,
    pub(super) before: String,
    pub(super) after: String,
}

#[derive(Deserialize)]
pub(super) struct AgentPermissionRequest {
    pub(super) operation_id: OperationId,
    pub(super) expected_revision: u64,
    pub(super) approval_detail_digest: Option<ApprovalDetailDigest>,
    pub(super) decision: PermissionDecision,
}

#[derive(Deserialize)]
pub(super) struct AgentLocalMcpLaunchRequest {
    pub(super) server_id: String,
}

#[derive(Deserialize)]
pub(super) struct AgentLocalMcpCallRequest {
    pub(super) server_id: String,
    pub(super) tool_name: String,
    #[serde(default)]
    pub(super) arguments: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
pub(super) struct AgentLocalMcpStatusResponse {
    pub(super) registered: Vec<RegisteredLocalMcpServer>,
    pub(super) active: Vec<LocalMcpServerRuntimeReceipt>,
}

#[derive(Serialize)]
pub(super) struct AgentLocalMcpOperationResponse {
    pub(super) operation_id: OperationId,
    pub(super) operation_revision: u64,
    pub(super) state: OperationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) runtime: Option<LocalMcpServerRuntimeReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) receipt: Option<LocalMcpToolCallReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(super) struct AgentTier2SourceRequest {
    pub(super) source_operation_id: OperationId,
}

#[derive(Serialize)]
pub(super) struct AgentTier2ResultsResponse {
    pub(super) results: Vec<AgentTier2ResultResponse>,
}

#[derive(Serialize)]
pub(super) struct AgentTier2ResultResponse {
    pub(super) source_operation_id: OperationId,
    pub(super) source_revision: String,
    pub(super) finished_at_ms: u64,
    pub(super) termination: IsolatedTaskTermination,
    pub(super) exit_code: Option<i32>,
    pub(super) changed_bytes: u64,
    pub(super) inventory_sha256: String,
    pub(super) changed_files: Vec<IsolatedChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) acceptance: Option<AgentTier2ReviewResponse>,
}

#[derive(Clone, Serialize)]
pub(super) struct AgentTier2ReviewResponse {
    pub(super) source_operation_id: OperationId,
    pub(super) operation_id: OperationId,
    pub(super) operation_revision: u64,
    pub(super) state: OperationState,
    pub(super) result_digest: String,
    pub(super) changed_file_count: usize,
}

#[derive(Serialize)]
pub(super) struct AgentTier2PreviewResponse {
    pub(super) source_operation_id: OperationId,
    pub(super) result_digest: String,
    pub(super) changes: Vec<AgentTier2PreviewChangeResponse>,
    pub(super) truncated: bool,
}

#[derive(Serialize)]
pub(super) struct AgentTier2PreviewChangeResponse {
    pub(super) target_path: String,
    pub(super) base_digest: Option<String>,
    pub(super) proposed_digest: String,
    pub(super) deleted: bool,
    pub(super) binary: bool,
    pub(super) base_bytes: u64,
    pub(super) proposed_bytes: u64,
    pub(super) hunks: Vec<AgentTier2PreviewHunkResponse>,
    pub(super) truncated: bool,
}

#[derive(Serialize)]
pub(super) struct AgentTier2PreviewHunkResponse {
    pub(super) id: String,
    pub(super) base_start: usize,
    pub(super) base_lines: usize,
    pub(super) proposed_start: usize,
    pub(super) proposed_lines: usize,
    pub(super) patch: String,
    pub(super) truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_projection_contains_only_lifecycle_identity() {
        let value = serde_json::to_value(AgentAttentionResponse {
            sessions: vec![AgentAttentionSession {
                session_id: 7,
                provider: "codex-acp".into(),
                status: AgentStatus::WaitingApproval,
                document_revision: 42,
            }],
        })
        .expect("serialize attention projection");
        assert_eq!(value["sessions"][0]["status"], "waiting_approval");
        assert!(value["sessions"][0].get("document").is_none());
        assert!(value["sessions"][0].get("error").is_none());
        assert!(value["sessions"][0].get("capabilities").is_none());
    }

    #[test]
    fn legacy_workspace_mapping_fields_remain_optional() {
        let request: AgentWorkspaceApplyRequest = serde_json::from_value(serde_json::json!({
            "artifact_source_revision": 3
        }))
        .expect("parse workspace request");
        assert_eq!(request.artifact_source_revision, 3);
        assert!(request.review_digest.is_none());
        assert!(request.source_path.is_none());
        assert!(request.target_path.is_none());
        assert!(request.mappings.is_empty());
    }
}
