use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{BlockId, EVENT_SCHEMA_VERSION, EventId, OperationId, RunId, TaskId, TerminalId};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User,
    Agent { adapter: String },
    Policy,
    System,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Agent,
    System,
    Thought,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    ReadOnly,
    WorkspaceWrite,
    ExternalEffect,
    Destructive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Shell,
    FileEdit,
    AgentTool,
    McpTool,
    ComputerUse,
    ArtifactBuild,
    Other(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationAction {
    Shell {
        command: TerminalCommand,
    },
    Opaque {
        kind: String,
        payload_digest: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Proposed,
    PolicyCheck,
    WaitingHuman,
    Authorized,
    Dispatching,
    Succeeded,
    Failed,
    Cancelled,
    UnknownExecution,
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationCompletion {
    pub executor: String,
    pub succeeded: bool,
    pub summary: String,
    pub result_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl TerminalSize {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.cols == 0 || self.rows == 0 {
            return Err("terminal rows and columns must be non-zero");
        }
        if self.cols > 1_000 || self.rows > 1_000 {
            return Err("terminal dimensions exceed the supported bound");
        }
        Ok(())
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 36,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalCommand {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl TerminalCommand {
    pub fn display_text(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainEvent {
    TaskCreated {
        title: String,
    },
    MessageAppended {
        block_id: BlockId,
        role: MessageRole,
        external_message_id: Option<String>,
        text: String,
    },
    OperationProposed {
        revision: u64,
        kind: OperationKind,
        action: OperationAction,
        summary: String,
        risk: RiskClass,
        required_capabilities: Vec<String>,
    },
    OperationStateChanged {
        revision: u64,
        from: OperationState,
        to: OperationState,
        actor: Actor,
        reason: Option<String>,
    },
    PermissionRequested {
        operation_revision: u64,
        prompt: String,
        options: Vec<PermissionDecision>,
    },
    PermissionDecided {
        operation_revision: u64,
        decision: PermissionDecision,
        actor: Actor,
    },
    OperationReceipt {
        operation_revision: u64,
        executor: String,
        succeeded: bool,
        summary: String,
        result_digest: Option<String>,
    },
    TerminalOpened {
        terminal_id: TerminalId,
        command: TerminalCommand,
        size: TerminalSize,
    },
    TerminalOutputObserved {
        terminal_id: TerminalId,
        stream_sequence: u64,
        byte_count: u64,
    },
    TerminalResized {
        terminal_id: TerminalId,
        generation: u64,
        size: TerminalSize,
    },
    TerminalExited {
        terminal_id: TerminalId,
        exit_code: Option<u32>,
    },
    ReviewReady {
        summary: String,
    },
    Diagnostic {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EventEnvelope {
    pub schema_version: u16,
    pub sequence: u64,
    pub event_id: EventId,
    pub recorded_at_ms: u64,
    pub task_id: TaskId,
    pub run_id: Option<RunId>,
    pub operation_id: Option<OperationId>,
    pub causation_id: Option<EventId>,
    pub correlation_id: Option<EventId>,
    pub payload: DomainEvent,
}

impl EventEnvelope {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err("unsupported event schema version");
        }
        if self.sequence == 0 {
            return Err("event sequence must start at one");
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct NewEvent {
    pub task_id: TaskId,
    pub run_id: Option<RunId>,
    pub operation_id: Option<OperationId>,
    pub causation_id: Option<EventId>,
    pub correlation_id: Option<EventId>,
    pub payload: DomainEvent,
}
