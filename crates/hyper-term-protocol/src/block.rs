use serde::{Deserialize, Serialize};

use crate::{
    AcceptedGenUiArtifact, BLOCK_SCHEMA_VERSION, BlockId, MessageRole, OperationId, OperationKind,
    OperationOutcome, OperationState, PermissionDecision, RiskClass, TaskId, TerminalId,
    TerminalSize,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Task,
    Message,
    Operation,
    Approval,
    Receipt,
    Artifact,
    Terminal,
    Review,
    Diagnostic,
    AgentToolCall,
    AgentPlan,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMediaKind {
    Image,
    Audio,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToolContent {
    Text {
        text: String,
    },
    Diff {
        path: String,
        patch: String,
        added_lines: u32,
        removed_lines: u32,
    },
    Terminal {
        terminal_id: String,
    },
    Media {
        kind: AgentMediaKind,
        mime_type: String,
        uri: Option<String>,
        encoded_bytes: u64,
    },
    Resource {
        name: String,
        uri: String,
        mime_type: Option<String>,
        text: Option<String>,
        byte_count: Option<u64>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentToolLocation {
    pub path: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentToolCall {
    pub tool_call_id: String,
    pub title: String,
    pub kind: AgentToolKind,
    pub status: AgentToolStatus,
    pub content: Vec<AgentToolContent>,
    pub locations: Vec<AgentToolLocation>,
    pub raw_input: Option<String>,
    pub raw_output: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlanPriority {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentPlanEntry {
    pub content: String,
    pub priority: AgentPlanPriority,
    pub status: AgentPlanStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSlot {
    SessionHeader,
    Timeline,
    Attention,
    Inspector,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    TrustedChrome,
    UntrustedContent,
    TrustedWorkbench,
    IsolatedArtifact,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockLifecycle {
    Draft,
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    UnknownExecution,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionState {
    None,
    WaitingInput,
    WaitingApproval,
    Failed,
    ReviewReady,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockAction {
    pub action_id: String,
    pub expected_block_revision: u64,
    pub risk: RiskClass,
    pub required_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockPayload {
    Task {
        title: String,
    },
    Message {
        role: MessageRole,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_message_id: Option<String>,
    },
    Operation {
        operation_id: OperationId,
        kind: OperationKind,
        summary: String,
        risk: RiskClass,
        #[serde(default)]
        required_capabilities: Vec<String>,
        state: OperationState,
    },
    Approval {
        operation_id: OperationId,
        operation_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval: Option<crate::BoundApprovalDetail>,
        prompt: String,
        options: Vec<PermissionDecision>,
        decision: Option<PermissionDecision>,
    },
    OperationReceipt {
        operation_id: OperationId,
        operation_revision: u64,
        executor: String,
        succeeded: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<OperationOutcome>,
        summary: String,
        result_digest: Option<String>,
    },
    Artifact {
        artifact: AcceptedGenUiArtifact,
    },
    Terminal {
        terminal_id: TerminalId,
        command: String,
        size: TerminalSize,
        resize_generation: u64,
        stream_sequence: u64,
        byte_count: u64,
        exit_code: Option<u32>,
    },
    Review {
        summary: String,
    },
    Diagnostic {
        code: String,
        message: String,
    },
    AgentToolCall {
        turn_id: String,
        call: AgentToolCall,
    },
    AgentPlan {
        turn_id: String,
        entries: Vec<AgentPlanEntry>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockEnvelope {
    pub schema_version: u16,
    pub block_id: BlockId,
    pub block_revision: u64,
    pub document_revision: u64,
    pub parent_block_id: Option<BlockId>,
    pub order_key: u64,
    pub task_id: TaskId,
    pub kind: BlockKind,
    pub render_slot: RenderSlot,
    pub trust_class: TrustClass,
    pub lifecycle: BlockLifecycle,
    pub attention: AttentionState,
    pub payload: BlockPayload,
    pub actions: Vec<BlockAction>,
}

impl BlockEnvelope {
    pub fn new(
        block_id: BlockId,
        task_id: TaskId,
        kind: BlockKind,
        order_key: u64,
        payload: BlockPayload,
    ) -> Self {
        Self {
            schema_version: BLOCK_SCHEMA_VERSION,
            block_id,
            block_revision: 1,
            document_revision: 0,
            parent_block_id: None,
            order_key,
            task_id,
            kind,
            render_slot: RenderSlot::Timeline,
            trust_class: TrustClass::UntrustedContent,
            lifecycle: BlockLifecycle::Draft,
            attention: AttentionState::None,
            payload,
            actions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockDocument {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub revision: u64,
    pub semantic_digest: String,
    pub blocks: Vec<BlockEnvelope>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockPatch {
    pub stream_sequence: u64,
    pub base_revision: u64,
    pub target_revision: u64,
    pub operations: Vec<BlockOperation>,
}

// Block operations are bounded wire values and are normally serialized or
// applied immediately. Keeping `Upsert` inline avoids a protocol-only heap
// allocation; the size difference is intentional and covered by frame bounds.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockOperation {
    Upsert {
        block: BlockEnvelope,
    },
    AppendContent {
        block_id: BlockId,
        expected_previous_revision: u64,
        block_revision: u64,
        text: String,
    },
    Remove {
        block_id: BlockId,
    },
}
