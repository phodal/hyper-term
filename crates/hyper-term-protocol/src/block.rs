use serde::{Deserialize, Serialize};

use crate::{
    BLOCK_SCHEMA_VERSION, BlockId, MessageRole, OperationId, OperationKind, OperationState,
    PermissionDecision, RiskClass, TaskId, TerminalId, TerminalSize,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Task,
    Message,
    Operation,
    Approval,
    Receipt,
    Terminal,
    Review,
    Diagnostic,
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
    },
    Operation {
        operation_id: OperationId,
        kind: OperationKind,
        summary: String,
        risk: RiskClass,
        state: OperationState,
    },
    Approval {
        operation_id: OperationId,
        operation_revision: u64,
        prompt: String,
        options: Vec<PermissionDecision>,
        decision: Option<PermissionDecision>,
    },
    OperationReceipt {
        operation_id: OperationId,
        operation_revision: u64,
        executor: String,
        succeeded: bool,
        summary: String,
        result_digest: Option<String>,
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
