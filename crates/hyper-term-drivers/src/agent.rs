use std::path::PathBuf;

use hyper_term_protocol::{AgentPlanEntry, AgentToolCall, OperationId, PermissionDecision};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DriverState;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionCapabilities {
    pub config_options: Vec<AgentSessionConfigOption>,
    pub available_commands: Vec<AgentAvailableCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionConfigOption {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub kind: AgentSessionConfigKind,
    pub choices: Vec<AgentSessionConfigChoice>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentSessionConfigKind {
    Select { current_value: String },
    Boolean { current_value: bool },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionConfigChoice {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
    pub group: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentAvailableCommand {
    pub name: String,
    pub description: String,
    pub input_hint: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentThreadGoal {
    pub objective: String,
    pub status: AgentGoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentSessionConfigValue {
    Id { value: String },
    Boolean { value: bool },
}

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentTerminalEnvironmentVariable {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentHostOperation {
    TerminalCreate {
        command: String,
        args: Vec<String>,
        env: Vec<AgentTerminalEnvironmentVariable>,
        cwd: PathBuf,
        output_byte_limit: u64,
    },
    TerminalOutput {
        terminal_id: String,
    },
    TerminalRelease {
        terminal_id: String,
    },
    TerminalWaitForExit {
        terminal_id: String,
    },
    TerminalKill {
        terminal_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentHostRequest {
    pub driver_id: Uuid,
    pub protocol: StructuredAgentProtocol,
    pub request_id: ExternalRequestId,
    pub method: String,
    pub payload_sha256: String,
    pub thread_id: String,
    pub turn_id: String,
    pub operation: AgentHostOperation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentHostResponse {
    TerminalCreated {
        terminal_id: String,
    },
    TerminalOutput {
        output: String,
        truncated: bool,
        exit_code: Option<u32>,
        signal: Option<String>,
    },
    TerminalReleased,
    TerminalExited {
        exit_code: Option<u32>,
        signal: Option<String>,
    },
    TerminalKilled,
    Error {
        code: i32,
        message: String,
    },
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
    PlanUpdated {
        sequence: u64,
        thread_id: String,
        turn_id: String,
        entries: Vec<AgentPlanEntry>,
    },
    ToolCallUpdated {
        sequence: u64,
        thread_id: String,
        turn_id: String,
        call: AgentToolCall,
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
    HostRequest {
        sequence: u64,
        request: AgentHostRequest,
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
