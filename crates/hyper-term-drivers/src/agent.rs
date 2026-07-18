use hyper_term_protocol::{OperationId, PermissionDecision};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DriverState;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredAgentProtocol {
    Acp,
    CodexAppServerV2,
    ClaudeStreamJson,
    Mcp20251125,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExternalRequestId {
    String(String),
    Signed(i64),
    Unsigned(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEffectKind {
    Shell,
    WorkspaceEdit,
    Tool,
    ComputerUse,
    Opaque,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentEffectProposal {
    pub driver_id: Uuid,
    pub protocol: StructuredAgentProtocol,
    pub request_id: ExternalRequestId,
    pub method: String,
    pub kind: AgentEffectKind,
    pub summary: String,
    pub required_capabilities: Vec<String>,
    pub payload_sha256: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentEffectAuthorization {
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub proposal_sha256: String,
    pub decision: PermissionDecision,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentDriverEvent {
    Connected {
        protocol: StructuredAgentProtocol,
        implementation_version: String,
    },
    MessageDelta {
        sequence: u64,
        thread_id: String,
        turn_id: String,
        text: String,
    },
    PlanDelta {
        sequence: u64,
        thread_id: String,
        turn_id: String,
        text: String,
    },
    ThoughtDelta {
        sequence: u64,
        thread_id: String,
        turn_id: String,
        text: String,
    },
    EffectProposed {
        sequence: u64,
        proposal: AgentEffectProposal,
    },
    TurnCompleted {
        sequence: u64,
        thread_id: String,
        turn_id: Option<String>,
        status: Option<String>,
    },
    ProtocolNotice {
        sequence: u64,
        method: Option<String>,
        payload_sha256: String,
    },
    Exited {
        code: Option<i32>,
        state: DriverState,
    },
}
