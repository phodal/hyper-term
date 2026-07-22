use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AcceptedGenUiArtifact, ActionDigest, AgentExecutionContextReceiptSet, AgentPlanEntry,
    AgentToolCall, BlockId, BoundApprovalDetail, CompiledSandboxProfile, EVENT_SCHEMA_VERSION,
    EventId, LocalMcpServerRuntimeReceipt, LocalMcpToolCall, LocalMcpToolCallReceipt, OperationId,
    RunId, SandboxLeaseId, SandboxProfileDigest, SandboxReceipt, SandboxViolation, TaskId,
    TerminalId,
};

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
    McpServerLaunch,
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
    McpServerLaunch {
        launch: crate::LocalMcpServerLaunch,
    },
    McpToolCall {
        call: LocalMcpToolCall,
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
    Violated,
    Cancelled,
    UnknownExecution,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Succeeded,
    Failed,
    UnknownExecution,
}

impl OperationOutcome {
    pub fn succeeded(self) -> bool {
        self == Self::Succeeded
    }
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Violated | Self::Cancelled
        )
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OperationOutcome>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval: Option<BoundApprovalDetail>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<OperationOutcome>,
        summary: String,
        result_digest: Option<String>,
    },
    ArtifactAccepted {
        artifact: AcceptedGenUiArtifact,
    },
    SandboxProfileCompiled {
        operation_revision: u64,
        compiled: CompiledSandboxProfile,
    },
    SandboxLeaseIssued {
        operation_revision: u64,
        lease_id: SandboxLeaseId,
        expires_at_ms: u64,
        profile_digest: SandboxProfileDigest,
        action_digest: ActionDigest,
    },
    SandboxReceiptRecorded {
        operation_revision: u64,
        receipt: SandboxReceipt,
    },
    SandboxViolationObserved {
        operation_revision: u64,
        violation: SandboxViolation,
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
    AgentToolCallUpdated {
        turn_id: String,
        call: AgentToolCall,
    },
    AgentPlanUpdated {
        turn_id: String,
        entries: Vec<AgentPlanEntry>,
    },
    AgentExecutionContextRecorded {
        context: AgentExecutionContextReceiptSet,
    },
    LocalMcpServerRuntimeRecorded {
        receipt: LocalMcpServerRuntimeReceipt,
    },
    LocalMcpToolCallRecorded {
        receipt: LocalMcpToolCallReceipt,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        ContextDigest, ContextReceipt, EXECUTION_CONTEXT_SCHEMA_VERSION, EnvironmentPlanDigest,
        ExecutionMode, LocalMcpCredentialScope, LocalMcpServerLaunch, LocalMcpServerLifecycle,
        LocalMcpServerRuntimeReceipt, LocalMcpToolCall, LocalMcpToolCallReceipt,
        LocalMcpToolContractReceipt, McpArgumentsDigest, McpCapabilitiesDigest, McpCatalogDigest,
        McpRuntimeIdentityDigest, McpToolContractDigest, McpToolResultDigest, SandboxProfileDigest,
    };

    #[test]
    fn legacy_boolean_receipts_remain_readable_without_an_outcome_field() {
        let completion: OperationCompletion = serde_json::from_value(json!({
            "executor": "legacy-mcp",
            "succeeded": false,
            "summary": "legacy failure",
            "result_digest": null
        }))
        .unwrap();
        assert!(!completion.succeeded);
        assert_eq!(completion.outcome, None);

        let event: DomainEvent = serde_json::from_value(json!({
            "type": "operation_receipt",
            "operation_revision": 4,
            "executor": "legacy-mcp",
            "succeeded": true,
            "summary": "legacy success",
            "result_digest": null
        }))
        .unwrap();
        assert!(matches!(
            event,
            DomainEvent::OperationReceipt {
                succeeded: true,
                outcome: None,
                ..
            }
        ));
    }

    #[test]
    fn agent_execution_context_receipts_serialize_as_redacted_evidence() {
        let event = DomainEvent::AgentExecutionContextRecorded {
            context: AgentExecutionContextReceiptSet {
                provider_id: "codex-acp".into(),
                protocol: "acp".into(),
                thread_id: "thread-1".into(),
                receipts: vec![ContextReceipt {
                    schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
                    context_id: "agent-provider".into(),
                    context_revision: 1,
                    mode: ExecutionMode::Hermetic,
                    context_digest: ContextDigest::parse("a".repeat(64)).unwrap(),
                    environment_digest: EnvironmentPlanDigest::parse("b".repeat(64)).unwrap(),
                    clear_inherited: true,
                    bindings: Vec::new(),
                    credential_bindings: Vec::new(),
                }],
            },
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "agent_execution_context_recorded");
        assert_eq!(value["context"]["receipts"][0]["mode"], "hermetic");
        assert!(value["context"]["receipts"][0].get("variables").is_none());
        assert!(value.to_string().find("secret_value").is_none());
    }

    #[test]
    fn local_mcp_runtime_receipts_serialize_as_redacted_evidence() {
        let launch = LocalMcpServerLaunch {
            schema_version: crate::LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
            server_id: "reviewed-files".into(),
            executable: "/usr/bin/reviewed-mcp".into(),
            executable_sha256: "a".repeat(64),
            arguments_digest: McpArgumentsDigest::parse("b".repeat(64)).unwrap(),
            argument_count: 2,
            working_directory: "/workspace".into(),
            context_digest: ContextDigest::parse("c".repeat(64)).unwrap(),
            sandbox_profile_digest: SandboxProfileDigest::parse("d".repeat(64)).unwrap(),
            roots_snapshot_sha256: Some("e".repeat(64)),
            lifecycle: LocalMcpServerLifecycle::OneTask,
            credential_scope: LocalMcpCredentialScope::ServerLifetime,
            runtime_identity_digest: McpRuntimeIdentityDigest::parse("f".repeat(64)).unwrap(),
        };
        let event = DomainEvent::LocalMcpServerRuntimeRecorded {
            receipt: LocalMcpServerRuntimeReceipt {
                schema_version: crate::LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
                launch,
                negotiated_protocol_version: "2025-11-25".into(),
                server_name: "reviewed-files".into(),
                server_version: "1.0.0".into(),
                enforced_sandbox_profile_digest: SandboxProfileDigest::parse("1".repeat(64))
                    .unwrap(),
                capabilities_digest: McpCapabilitiesDigest::parse("2".repeat(64)).unwrap(),
                catalog_digest: McpCatalogDigest::parse("3".repeat(64)).unwrap(),
                runtime_identity_digest: McpRuntimeIdentityDigest::parse("4".repeat(64)).unwrap(),
                tools: vec![LocalMcpToolContractReceipt {
                    name: "read_file".into(),
                    input_schema_sha256: "5".repeat(64),
                    output_schema_sha256: Some("6".repeat(64)),
                    contract_digest: McpToolContractDigest::parse("7".repeat(64)).unwrap(),
                }],
                credential_scope: LocalMcpCredentialScope::ServerLifetime,
                per_call_isolation: false,
            },
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "local_mcp_server_runtime_recorded");
        assert_eq!(value["receipt"]["tools"][0]["name"], "read_file");
        let encoded = value.to_string();
        assert!(!encoded.contains("--secret"));
        assert!(!encoded.contains("secret-token"));
        assert!(!encoded.contains("environment"));
        assert!(!encoded.contains("arguments\""));
    }

    #[test]
    fn local_mcp_tool_receipts_bind_identity_without_persisting_arguments() {
        let call = LocalMcpToolCall {
            schema_version: crate::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION,
            server_id: "reviewed-files".into(),
            runtime_identity_digest: McpRuntimeIdentityDigest::parse("1".repeat(64)).unwrap(),
            catalog_digest: McpCatalogDigest::parse("2".repeat(64)).unwrap(),
            tool_name: "read_file".into(),
            tool_contract_digest: McpToolContractDigest::parse("3".repeat(64)).unwrap(),
            arguments_digest: McpArgumentsDigest::parse("4".repeat(64)).unwrap(),
        };
        let event = DomainEvent::LocalMcpToolCallRecorded {
            receipt: LocalMcpToolCallReceipt {
                schema_version: crate::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION,
                call,
                succeeded: true,
                result_digest: McpToolResultDigest::parse("5".repeat(64)).unwrap(),
                content_count: 1,
                has_structured_content: true,
            },
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "local_mcp_tool_call_recorded");
        assert_eq!(value["receipt"]["call"]["tool_name"], "read_file");
        let encoded = value.to_string();
        assert!(!encoded.contains("argument_values"));
        assert!(!encoded.contains("secret-path"));
    }
}
