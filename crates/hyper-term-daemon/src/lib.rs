//! Out-of-process authority host for Hyper Term.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use hyper_term_core::{
    BlockProjector, CapabilityLeaseLedger, ExecutionAuthorizationError,
    ExecutionAuthorizationRequest, ExecutionContextInputs, JournalError, JsonlJournal,
    OperationError, OperationRecord, OperationReducer, ProjectorError, SandboxError,
    SandboxLaunchPlan, SandboxLauncher, SandboxLeaseError, SandboxLeaseExpectation, TerminalConfig,
    TerminalError, TerminalEvent, TerminalReplay, TerminalSessionHandle, TerminalSubscription,
    TerminalSupervisor, UserShellConfig, compile_authorized_execution_plan,
};
use hyper_term_protocol::{
    AcceptedGenUiArtifact, Actor, AgentExecutionContextReceiptSet, BlockDocument, BlockId,
    BlockPatch, ClientId, CompiledSandboxProfile, ContextDigest, ContextReceipt, ControlRequest,
    ControlRequestEnvelope, ControlResponse, ControlResponseEnvelope, DomainEvent,
    EXECUTION_CONTEXT_SCHEMA_VERSION, EventEnvelope, ExecutionCapabilityLease,
    GenUiArtifactCandidate, InputLeaseId, LocalMcpServerRuntimeReceipt, LocalMcpToolCall,
    LocalMcpToolCallReceipt, MessageRole, NewEvent, OperationAction, OperationCompletion,
    OperationId, OperationKind, OperationOutcome, OperationState, PROTOCOL_VERSION,
    PermissionDecision, RequestId, RiskClass, SandboxEnforcement, SandboxEnvironmentPolicy,
    SandboxFileSystemPolicy, SandboxLeaseId, SandboxLifetime, SandboxNetworkPolicy, SandboxOutcome,
    SandboxPathAccess, SandboxPathRule, SandboxProcessPolicy, SandboxReceipt,
    SandboxResourceLimits, TaskId, TerminalDataFrame, TerminalId, TerminalInputFrame, TerminalSize,
    TerminalSnapshotFrame, WireError, WireFrame, read_frame, write_frame,
};
use hyper_term_sandbox::{
    IsolatedTaskReceipt, IsolatedTaskRequest, IsolatedTaskTermination, IsolatedWorktreeError,
    IsolatedWorktreeManager, IsolatedWorktreeRequest, LimaIsolatedTaskLauncher, LimaRunnerError,
    LimaTaskRunner, MacOsSeatbeltLauncher,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

mod acp_provider_home;
#[cfg(unix)]
mod agent_capability;
mod agent_gateway;
mod agent_provider_probe;
mod agent_session_store;
mod approval_detail;
mod artifact_debug_capsule;
mod artifact_editor_store;
mod artifact_runtime_trace_store;
mod artifact_store;
mod artifact_visual_quality_store;
#[cfg(unix)]
mod brokered_mcp_operation;
mod claude_credentials;
#[cfg(unix)]
mod client;
mod copilot_credentials;
mod daemon_isolated;
mod daemon_terminal;
mod daemon_validation;
mod editor_lsp;
mod isolated_result_store;
mod local_mcp_runtime;
#[cfg(unix)]
mod mcp_gateway;
mod network_proxy;
mod operation_execution_context;
mod private_fs;
#[cfg(unix)]
mod state_root_lock;
mod web_gateway;
mod workspace_apply;
mod workspace_diff;
mod workspace_snapshot;
use approval_detail::{bound_approval_detail, validate_action_kind};
use artifact_store::{ArtifactStore, ArtifactStoreError, StoredGenUiArtifact};
#[cfg(unix)]
use brokered_mcp_operation::CachedBrokeredMcpExecution;
use daemon_validation::*;
use isolated_result_store::{
    IsolatedAcceptance, IsolatedResult, PreparedIsolatedAcceptance, StoredIsolatedAcceptance,
    isolated_acceptance_changes, isolated_acceptance_digest, isolated_acceptance_summary,
    recover_completed_isolated_results, recover_isolated_acceptances, remove_isolated_acceptance,
    safe_isolated_result_path, write_isolated_acceptance,
};
#[cfg(unix)]
use state_root_lock::StateRootLock;
use workspace_apply::{
    DurableWorkspaceApplyResult, WorkspaceApplyRequest, WorkspaceTransactionContext,
    WorkspaceTransactionOutcome, acknowledge_workspace_transaction,
    apply_workspace_set_plan_durable, prepare_workspace_apply_requests,
};

pub use artifact_debug_capsule::{BugCapsuleError, load_bug_capsule};
pub use isolated_result_store::{
    IsolatedAcceptanceChange, IsolatedAcceptancePreview, IsolatedAcceptanceReview,
    IsolatedResultReview,
};

#[cfg(unix)]
pub use agent_capability::AgentCapabilityPolicy;
pub use agent_gateway::{
    AcpAgentProviderConfig, AgentGatewayConfig, AgentGatewayError, AgentGatewayHandle,
    AgentGenUiRuntimeConfig, AgentProviderContainment, AgentProviderReadiness, AgentProviderStatus,
    probe_agent_provider_statuses, spawn_agent_gateway,
};
#[cfg(unix)]
pub use client::{ControlClient, ControlClientError};
pub use local_mcp_runtime::{
    LocalMcpRuntimeError, LocalMcpRuntimeManager, RegisteredLocalMcpServer,
};
#[cfg(unix)]
pub use mcp_gateway::{
    BrokeredMcpRuntimeConfig, DenoGenUiMcpExecutorConfig, DenoMcpExecutorConfig, McpGatewayError,
    McpStdioConfig, run_mcp_stdio,
};
pub use web_gateway::{
    DESKTOP_WORKSPACE_STATE_FILE, DesktopSessionSnapshot, DesktopWorkspaceSnapshot,
    DesktopWorkspaceStore, TerminalGatewayConfig, TerminalGatewayError, TerminalGatewayHandle,
    spawn_terminal_gateway,
};

const CONTROL_SUBSCRIBER_CAPACITY: usize = 512;
const BLOCK_SUBSCRIBER_CAPACITY: usize = 512;
const OBSERVATION_BATCH_BYTES: u64 = 64 * 1024;
const SANDBOX_LEASE_TTL_MS: u64 = 5 * 60 * 1_000;
const MAX_GENUI_ARTIFACT_HISTORY: usize = 64;
const ISOLATED_TASK_CAPABILITY: &str = "sandbox.isolated_task";
const ISOLATED_TASK_WALL_TIME_MS: u64 = 10 * 60 * 1_000;
const ISOLATED_TASK_MAX_PROCESSES: u32 = 256;
const ISOLATED_TASK_MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024;
const ISOLATED_ACCEPTANCE_SCHEMA_VERSION: u32 = 1;
const ISOLATED_ACCEPTANCE_DIRECTORY: &str = "isolated-acceptances";
const MAX_ISOLATED_ACCEPTANCE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GenUiArtifactHistoryEntry {
    pub event_sequence: u64,
    pub recorded_at_ms: u64,
    pub operation_id: Option<OperationId>,
    pub artifact: AcceptedGenUiArtifact,
}

#[derive(Clone)]
pub struct DaemonState {
    inner: Arc<DaemonInner>,
}

struct DaemonInner {
    instance_id: Uuid,
    #[cfg(unix)]
    _state_root_lock: StateRootLock,
    authority: Mutex<AuthorityState>,
    terminals: TerminalSupervisor,
    terminal_contexts: Mutex<HashMap<TerminalId, TerminalContext>>,
    input_leases: Mutex<HashMap<TerminalId, InputLease>>,
    lease_generations: Mutex<HashMap<TerminalId, u64>>,
    cancelled_terminals: Mutex<HashSet<TerminalId>>,
    control_subscribers: Mutex<Vec<Sender<ControlResponse>>>,
    block_subscribers: Mutex<Vec<Sender<(TaskId, BlockPatch)>>>,
    sandbox_launcher: Arc<dyn SandboxLauncher>,
    sandbox_leases: Mutex<CapabilityLeaseLedger>,
    authorized_sandboxes: Mutex<HashMap<OperationId, AuthorizedSandbox>>,
    sandbox_executions: Mutex<HashMap<TerminalId, SandboxExecutionContext>>,
    isolated_worktree_manager: IsolatedWorktreeManager,
    isolated_results: Mutex<HashMap<OperationId, IsolatedResult>>,
    isolated_results_root: PathBuf,
    isolated_acceptances: Mutex<HashMap<OperationId, IsolatedAcceptance>>,
    isolated_acceptances_root: PathBuf,
    state_directory: PathBuf,
    artifacts: ArtifactStore,
    artifact_acceptance: Mutex<()>,
    scratch_root: PathBuf,
    #[cfg(unix)]
    brokered_mcp_runtimes: Mutex<HashMap<TaskId, Arc<Mutex<mcp_gateway::BrokeredMcpExecutor>>>>,
    #[cfg(unix)]
    brokered_mcp_executions: Mutex<HashMap<OperationId, CachedBrokeredMcpExecution>>,
}

impl Drop for DaemonInner {
    fn drop(&mut self) {
        cleanup_scratch_directory(&self.scratch_root);
    }
}

struct AuthorityState {
    journal: JsonlJournal,
    operations: OperationReducer,
    projectors: HashMap<TaskId, BlockProjector>,
}

#[derive(Clone, Copy)]
enum TerminalContext {
    Operation(OperationTerminalContext),
    UserShell,
}

#[derive(Clone, Copy)]
struct OperationTerminalContext {
    task_id: TaskId,
    operation_id: OperationId,
}

#[derive(Clone)]
struct AuthorizedSandbox {
    lease_id: SandboxLeaseId,
    context_digest: ContextDigest,
    plan: SandboxLaunchPlan,
    scratch_directory: PathBuf,
}

struct PreparedSandbox {
    authorized: AuthorizedSandbox,
    lease: ExecutionCapabilityLease,
    context_receipt: ContextReceipt,
}

#[derive(Clone)]
struct SandboxExecutionContext {
    compiled: CompiledSandboxProfile,
    started_at_ms: u64,
    scratch_directory: PathBuf,
}

#[derive(Clone, Copy)]
struct InputLease {
    lease_id: InputLeaseId,
    client_id: ClientId,
    generation: u64,
}

impl DaemonState {
    pub fn open(state_directory: impl AsRef<Path>) -> Result<Self, DaemonError> {
        Self::open_with_sandbox_launcher(state_directory, Arc::new(MacOsSeatbeltLauncher))
    }

    fn open_with_sandbox_launcher(
        state_directory: impl AsRef<Path>,
        sandbox_launcher: Arc<dyn SandboxLauncher>,
    ) -> Result<Self, DaemonError> {
        create_private_directory(state_directory.as_ref())?;
        let state_directory = fs::canonicalize(state_directory.as_ref())?;
        #[cfg(unix)]
        let state_root_lock = StateRootLock::acquire(&state_directory)?;
        let artifacts = ArtifactStore::open(&state_directory)?;
        let isolated_worktree_manager = IsolatedWorktreeManager::discover()?;
        let isolated_results_root = state_directory.join("isolated-results");
        create_private_directory(&isolated_results_root)?;
        let isolated_acceptances_root = state_directory.join(ISOLATED_ACCEPTANCE_DIRECTORY);
        create_private_directory(&isolated_acceptances_root)?;
        let journal = JsonlJournal::open(state_directory.join("events.jsonl"))?;
        let mut operations = OperationReducer::default();
        let mut projectors = HashMap::new();
        let mut created_tasks = HashSet::new();

        for event in journal.all() {
            if matches!(event.payload, DomainEvent::TaskCreated { .. }) {
                if !created_tasks.insert(event.task_id) {
                    return Err(DaemonError::DuplicateTask(event.task_id));
                }
            } else if !created_tasks.contains(&event.task_id) {
                return Err(DaemonError::JournalEventBeforeTask(event.task_id));
            }
            operations.apply(event)?;
            projectors
                .entry(event.task_id)
                .or_insert_with(|| BlockProjector::new(event.task_id))
                .apply(event)?;
            if let DomainEvent::ArtifactAccepted { artifact } = &event.payload {
                artifacts.read(artifact)?;
            }
        }

        let isolated_results = recover_completed_isolated_results(
            &isolated_worktree_manager,
            &isolated_results_root,
            &operations,
        )?;
        let isolated_acceptances = recover_isolated_acceptances(
            &isolated_acceptances_root,
            &operations,
            &isolated_results,
        )?;
        let instance_id = Uuid::new_v4();
        let scratch_root = std::env::temp_dir()
            .join("hyper-term-agent")
            .join(instance_id.to_string());
        create_private_directory(&scratch_root)?;
        let scratch_root = fs::canonicalize(scratch_root)?;
        let daemon = Self {
            inner: Arc::new(DaemonInner {
                instance_id,
                #[cfg(unix)]
                _state_root_lock: state_root_lock,
                authority: Mutex::new(AuthorityState {
                    journal,
                    operations,
                    projectors,
                }),
                terminals: TerminalSupervisor::default(),
                terminal_contexts: Mutex::new(HashMap::new()),
                input_leases: Mutex::new(HashMap::new()),
                lease_generations: Mutex::new(HashMap::new()),
                cancelled_terminals: Mutex::new(HashSet::new()),
                control_subscribers: Mutex::new(Vec::new()),
                block_subscribers: Mutex::new(Vec::new()),
                sandbox_launcher,
                sandbox_leases: Mutex::new(CapabilityLeaseLedger::default()),
                authorized_sandboxes: Mutex::new(HashMap::new()),
                sandbox_executions: Mutex::new(HashMap::new()),
                isolated_worktree_manager,
                isolated_results: Mutex::new(isolated_results),
                isolated_results_root,
                isolated_acceptances: Mutex::new(isolated_acceptances),
                isolated_acceptances_root,
                state_directory,
                artifacts,
                artifact_acceptance: Mutex::new(()),
                scratch_root,
                #[cfg(unix)]
                brokered_mcp_runtimes: Mutex::new(HashMap::new()),
                #[cfg(unix)]
                brokered_mcp_executions: Mutex::new(HashMap::new()),
            }),
        };
        daemon.reconcile_interrupted_dispatches()?;
        daemon.reconcile_unrecoverable_sandbox_authorizations()?;
        Ok(daemon)
    }

    pub fn instance_id(&self) -> Uuid {
        self.inner.instance_id
    }

    pub fn create_task(&self, title: String) -> Result<TaskId, DaemonError> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(DaemonError::EmptyTaskTitle);
        }
        let task_id = TaskId::new();
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: None,
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::TaskCreated { title },
        })?;
        Ok(task_id)
    }

    pub fn append_message(
        &self,
        task_id: TaskId,
        block_id: BlockId,
        role: MessageRole,
        external_message_id: Option<String>,
        text: String,
    ) -> Result<(), DaemonError> {
        self.require_task(task_id)?;
        if text.is_empty() {
            return Err(DaemonError::EmptyMessage);
        }
        if text.len() > 64 * 1024 {
            return Err(DaemonError::MessageTooLarge(text.len()));
        }
        if external_message_id
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
        {
            return Err(DaemonError::ExternalMessageIdTooLarge);
        }
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: None,
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::MessageAppended {
                block_id,
                role,
                external_message_id,
                text,
            },
        })
    }

    pub fn record_agent_execution_context(
        &self,
        task_id: TaskId,
        provider_id: String,
        protocol: String,
        thread_id: String,
        receipts: Vec<ContextReceipt>,
    ) -> Result<(), DaemonError> {
        self.require_task(task_id)?;
        validate_agent_execution_context(&provider_id, &protocol, &thread_id, &receipts)?;
        let task_created_event_id = {
            let authority = lock(&self.inner.authority)?;
            authority
                .journal
                .all()
                .iter()
                .find(|event| {
                    event.task_id == task_id
                        && matches!(event.payload, DomainEvent::TaskCreated { .. })
                })
                .map(|event| event.event_id)
                .ok_or(DaemonError::TaskNotFound(task_id))?
        };
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: None,
            causation_id: Some(task_created_event_id),
            correlation_id: Some(task_created_event_id),
            payload: DomainEvent::AgentExecutionContextRecorded {
                context: AgentExecutionContextReceiptSet {
                    provider_id,
                    protocol,
                    thread_id,
                    receipts,
                },
            },
        })
    }

    pub fn agent_execution_context_event(
        &self,
        task_id: TaskId,
    ) -> Result<Option<EventEnvelope>, DaemonError> {
        self.require_task(task_id)?;
        let authority = lock(&self.inner.authority)?;
        Ok(authority
            .journal
            .all()
            .iter()
            .rev()
            .find(|event| {
                event.task_id == task_id
                    && matches!(
                        event.payload,
                        DomainEvent::AgentExecutionContextRecorded { .. }
                    )
            })
            .cloned())
    }

    pub fn record_local_mcp_server_runtime(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        receipt: LocalMcpServerRuntimeReceipt,
    ) -> Result<(), DaemonError> {
        let operation = self.operation(operation_id)?;
        validate_operation_scope(&operation, task_id, expected_revision)?;
        if operation.state != OperationState::Dispatching {
            return Err(DaemonError::OperationNotDispatching(operation.state));
        }
        let OperationAction::McpServerLaunch { launch } = &operation.action else {
            return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
        };
        if operation.kind != OperationKind::McpServerLaunch || &receipt.launch != launch {
            return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
        }
        validate_local_mcp_runtime_receipt(&receipt)?;
        let proposal_event_id = {
            let authority = lock(&self.inner.authority)?;
            if authority.journal.all().iter().any(|event| {
                event.task_id == task_id
                    && event.operation_id == Some(operation_id)
                    && matches!(
                        event.payload,
                        DomainEvent::LocalMcpServerRuntimeRecorded { .. }
                    )
            }) {
                return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
            }
            authority
                .journal
                .all()
                .iter()
                .find(|event| {
                    event.task_id == task_id
                        && event.operation_id == Some(operation_id)
                        && matches!(event.payload, DomainEvent::OperationProposed { .. })
                })
                .map(|event| event.event_id)
                .ok_or(DaemonError::OperationNotFound(operation_id))?
        };
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: Some(proposal_event_id),
            correlation_id: Some(proposal_event_id),
            payload: DomainEvent::LocalMcpServerRuntimeRecorded { receipt },
        })
    }

    pub fn local_mcp_server_runtime_event(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
    ) -> Result<Option<EventEnvelope>, DaemonError> {
        let operation = self.operation(operation_id)?;
        if operation.task_id != task_id {
            return Err(DaemonError::OperationTaskMismatch {
                expected: operation.task_id,
                actual: task_id,
            });
        }
        let authority = lock(&self.inner.authority)?;
        Ok(authority
            .journal
            .all()
            .iter()
            .rev()
            .find(|event| {
                event.task_id == task_id
                    && event.operation_id == Some(operation_id)
                    && matches!(
                        event.payload,
                        DomainEvent::LocalMcpServerRuntimeRecorded { .. }
                    )
            })
            .cloned())
    }

    pub fn record_local_mcp_tool_call(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        receipt: LocalMcpToolCallReceipt,
    ) -> Result<(), DaemonError> {
        let operation = self.operation(operation_id)?;
        validate_operation_scope(&operation, task_id, expected_revision)?;
        if operation.state != OperationState::Dispatching {
            return Err(DaemonError::OperationNotDispatching(operation.state));
        }
        let OperationAction::McpToolCall { call } = &operation.action else {
            return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
        };
        if operation.kind != OperationKind::McpTool || &receipt.call != call {
            return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
        }
        validate_local_mcp_tool_call_receipt(&receipt)?;
        let (proposal_event_id, runtime_event_id) = {
            let authority = lock(&self.inner.authority)?;
            if authority.journal.all().iter().any(|event| {
                event.task_id == task_id
                    && event.operation_id == Some(operation_id)
                    && matches!(event.payload, DomainEvent::LocalMcpToolCallRecorded { .. })
            }) {
                return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
            }
            let proposal_event_id = authority
                .journal
                .all()
                .iter()
                .find(|event| {
                    event.task_id == task_id
                        && event.operation_id == Some(operation_id)
                        && matches!(event.payload, DomainEvent::OperationProposed { .. })
                })
                .map(|event| event.event_id)
                .ok_or(DaemonError::OperationNotFound(operation_id))?;
            let runtime_event_id = authority
                .journal
                .all()
                .iter()
                .rev()
                .find_map(|event| match &event.payload {
                    DomainEvent::LocalMcpServerRuntimeRecorded { receipt: runtime }
                        if event.task_id == task_id
                            && runtime.launch.server_id == call.server_id
                            && runtime.runtime_identity_digest == call.runtime_identity_digest
                            && runtime.catalog_digest == call.catalog_digest
                            && runtime.tools.iter().any(|tool| {
                                tool.name == call.tool_name
                                    && tool.contract_digest == call.tool_contract_digest
                            }) =>
                    {
                        Some(event.event_id)
                    }
                    _ => None,
                })
                .ok_or(DaemonError::LocalMcpRuntimeNotRecorded)?;
            (proposal_event_id, runtime_event_id)
        };
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: Some(proposal_event_id),
            correlation_id: Some(runtime_event_id),
            payload: DomainEvent::LocalMcpToolCallRecorded { receipt },
        })
    }

    pub fn local_mcp_tool_call_event(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
    ) -> Result<Option<EventEnvelope>, DaemonError> {
        let operation = self.operation(operation_id)?;
        if operation.task_id != task_id {
            return Err(DaemonError::OperationTaskMismatch {
                expected: operation.task_id,
                actual: task_id,
            });
        }
        let authority = lock(&self.inner.authority)?;
        Ok(authority
            .journal
            .all()
            .iter()
            .rev()
            .find(|event| {
                event.task_id == task_id
                    && event.operation_id == Some(operation_id)
                    && matches!(event.payload, DomainEvent::LocalMcpToolCallRecorded { .. })
            })
            .cloned())
    }

    pub fn update_agent_tool_call(
        &self,
        task_id: TaskId,
        turn_id: String,
        call: hyper_term_protocol::AgentToolCall,
    ) -> Result<(), DaemonError> {
        self.require_task(task_id)?;
        if turn_id.is_empty() || turn_id.len() > 4096 {
            return Err(DaemonError::InvalidAgentProjection(
                "Agent turn id is empty or too large".into(),
            ));
        }
        validate_agent_tool_call(&call)?;
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: None,
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::AgentToolCallUpdated { turn_id, call },
        })
    }

    pub fn update_agent_plan(
        &self,
        task_id: TaskId,
        turn_id: String,
        entries: Vec<hyper_term_protocol::AgentPlanEntry>,
    ) -> Result<(), DaemonError> {
        self.require_task(task_id)?;
        if turn_id.is_empty() || turn_id.len() > 4096 || entries.len() > 128 {
            return Err(DaemonError::InvalidAgentProjection(
                "Agent plan identity or entry count is invalid".into(),
            ));
        }
        if entries
            .iter()
            .any(|entry| entry.content.is_empty() || entry.content.len() > 16 * 1024)
        {
            return Err(DaemonError::InvalidAgentProjection(
                "Agent plan entry is empty or too large".into(),
            ));
        }
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: None,
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::AgentPlanUpdated { turn_id, entries },
        })
    }

    pub fn propose_operation(
        &self,
        task_id: TaskId,
        kind: OperationKind,
        action: OperationAction,
        summary: String,
        risk: RiskClass,
        required_capabilities: Vec<String>,
    ) -> Result<OperationRecord, DaemonError> {
        self.require_task(task_id)?;
        validate_action_kind(&kind, &action)?;
        let operation_id = OperationId::new();
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::OperationProposed {
                revision: 1,
                kind,
                action,
                summary,
                risk,
                required_capabilities,
            },
        })?;
        self.transition(
            task_id,
            operation_id,
            1,
            OperationState::PolicyCheck,
            Actor::Policy,
            Some("operation entered the permission policy pipeline".into()),
        )?;
        let waiting = self.transition(
            task_id,
            operation_id,
            2,
            OperationState::WaitingHuman,
            Actor::Policy,
            Some("M1 requires explicit human approval for every effect".into()),
        )?;
        let approval = bound_approval_detail(&waiting)?;
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::PermissionRequested {
                operation_revision: waiting.revision,
                approval: Some(approval),
                prompt: "Allow this exact operation once?".into(),
                options: vec![
                    PermissionDecision::AllowOnce,
                    PermissionDecision::RejectOnce,
                    PermissionDecision::Cancelled,
                ],
            },
        })?;
        // Return the state produced by this request. A fast permission
        // decision may advance the shared operation before this function
        // returns, but that later transition belongs on the ordered event
        // stream rather than in the proposal response.
        Ok(waiting)
    }

    pub fn decide_permission(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        decision: PermissionDecision,
    ) -> Result<OperationRecord, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if record.state != OperationState::WaitingHuman {
            return Err(DaemonError::OperationNotWaiting(record.state));
        }
        let prepared_sandbox = if matches!(
            decision,
            PermissionDecision::AllowOnce | PermissionDecision::AllowAlways
        ) && matches!(record.action, OperationAction::Shell { .. })
        {
            Some(self.prepare_authorized_sandbox(&record, expected_revision + 1)?)
        } else {
            None
        };
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::PermissionDecided {
                operation_revision: expected_revision,
                decision,
                actor: Actor::User,
            },
        })?;
        let next_state = match decision {
            PermissionDecision::AllowOnce | PermissionDecision::AllowAlways => {
                OperationState::Authorized
            }
            PermissionDecision::RejectOnce
            | PermissionDecision::RejectAlways
            | PermissionDecision::Cancelled => OperationState::Cancelled,
        };
        let updated = self.transition(
            task_id,
            operation_id,
            expected_revision,
            next_state,
            Actor::User,
            Some(format!("permission decision: {decision:?}")),
        )?;
        if let Some(prepared) = prepared_sandbox {
            self.activate_authorized_sandbox(task_id, updated.revision, prepared)?;
        }
        self.operation(operation_id)
    }

    fn prepare_authorized_sandbox(
        &self,
        record: &OperationRecord,
        authorized_revision: u64,
    ) -> Result<PreparedSandbox, DaemonError> {
        let OperationAction::Shell { command } = &record.action else {
            return Err(DaemonError::UnsupportedTerminalAction);
        };
        let workspace = command
            .cwd
            .as_deref()
            .ok_or(DaemonError::SandboxWorkingDirectoryRequired)?;
        let workspace = fs::canonicalize(workspace).map_err(|error| {
            DaemonError::InvalidSandboxWorkingDirectory {
                path: workspace.to_path_buf(),
                message: error.to_string(),
            }
        })?;
        if !workspace.is_dir() {
            return Err(DaemonError::InvalidSandboxWorkingDirectory {
                path: workspace,
                message: "not a directory".into(),
            });
        }
        if workspace.starts_with(&self.inner.state_directory) {
            return Err(DaemonError::WorkspaceInsideDaemonState);
        }
        let isolated_task = record
            .required_capabilities
            .iter()
            .any(|capability| capability == ISOLATED_TASK_CAPABILITY);

        let scratch_directory = if isolated_task {
            self.inner
                .isolated_results_root
                .join(record.operation_id.to_string())
        } else {
            self.inner
                .scratch_root
                .join(record.operation_id.to_string())
        };
        create_private_directory(&scratch_directory)?;
        let scratch_directory = fs::canonicalize(scratch_directory)?;

        let result = (|| {
            let mut rules = ["/System", "/usr", "/bin", "/sbin", "/Library"]
                .into_iter()
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .map(|path| SandboxPathRule {
                    path,
                    access: SandboxPathAccess::Read,
                })
                .collect::<Vec<_>>();
            rules.push(SandboxPathRule {
                path: workspace.clone(),
                access: if record.risk == RiskClass::ReadOnly {
                    SandboxPathAccess::Read
                } else {
                    SandboxPathAccess::Write
                },
            });
            for metadata in [".git", ".hg", ".svn", ".jj"] {
                rules.push(SandboxPathRule {
                    path: workspace.join(metadata),
                    access: SandboxPathAccess::Read,
                });
            }
            rules.push(SandboxPathRule {
                path: self.inner.state_directory.clone(),
                access: SandboxPathAccess::Deny,
            });
            rules.push(SandboxPathRule {
                path: scratch_directory.clone(),
                access: SandboxPathAccess::Write,
            });

            let profile = hyper_term_protocol::SandboxProfile {
                enforcement: if isolated_task {
                    SandboxEnforcement::IsolatedTask
                } else {
                    SandboxEnforcement::Native
                },
                filesystem: SandboxFileSystemPolicy { rules },
                network: SandboxNetworkPolicy::Offline,
                environment: SandboxEnvironmentPolicy::default(),
                platform: Default::default(),
                process: SandboxProcessPolicy {
                    allow_child_processes: true,
                    allow_any_executable: true,
                    allowed_executables: Vec::new(),
                },
                resources: if isolated_task {
                    SandboxResourceLimits {
                        wall_time_ms: Some(ISOLATED_TASK_WALL_TIME_MS),
                        max_processes: Some(ISOLATED_TASK_MAX_PROCESSES),
                        max_output_bytes: Some(ISOLATED_TASK_MAX_OUTPUT_BYTES),
                    }
                } else {
                    SandboxResourceLimits::default()
                },
                lifetime: SandboxLifetime::OneOperation,
            };
            let actor = Actor::System;
            let mut normalized_command = command.clone();
            normalized_command.cwd = Some(workspace.clone());
            let context = operation_execution_context::operation_execution_context_spec(
                record.operation_id,
                authorized_revision,
                &normalized_command,
                &workspace,
                &scratch_directory,
                profile,
            );
            let issued_at_ms = now_ms()?;
            let request = ExecutionAuthorizationRequest {
                operation_id: record.operation_id,
                operation_revision: authorized_revision,
                actor,
                context,
                command: normalized_command,
                lease_id: SandboxLeaseId::new(),
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(SANDBOX_LEASE_TTL_MS),
            };
            let authorization = if isolated_task {
                compile_authorized_execution_plan(
                    request,
                    &ExecutionContextInputs::default(),
                    &LimaIsolatedTaskLauncher,
                )?
            } else {
                compile_authorized_execution_plan(
                    request,
                    &ExecutionContextInputs::default(),
                    self.inner.sandbox_launcher.as_ref(),
                )?
            };
            if !authorization.sandbox.compiled.enforced
                || authorization.sandbox.compiled.backend
                    == hyper_term_protocol::SandboxBackendKind::TestOnlyUnenforced
            {
                return Err(DaemonError::UnenforcedSandboxBackend);
            }
            let lease = authorization.capability_lease;
            Ok(PreparedSandbox {
                authorized: AuthorizedSandbox {
                    lease_id: lease.lease.lease_id,
                    context_digest: lease.context_digest.clone(),
                    plan: authorization.sandbox,
                    scratch_directory: scratch_directory.clone(),
                },
                lease,
                context_receipt: authorization.context_receipt,
            })
        })();
        if result.is_err() {
            cleanup_scratch_directory(&scratch_directory);
        }
        result
    }

    fn activate_authorized_sandbox(
        &self,
        task_id: TaskId,
        operation_revision: u64,
        prepared: PreparedSandbox,
    ) -> Result<(), DaemonError> {
        let operation_id = prepared.lease.lease.operation_id;
        let activation = (|| {
            self.record(NewEvent {
                task_id,
                run_id: None,
                operation_id: Some(operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::OperationExecutionContextCompiled {
                    operation_revision,
                    receipt: prepared.context_receipt.clone(),
                },
            })?;
            self.record(NewEvent {
                task_id,
                run_id: None,
                operation_id: Some(operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::SandboxProfileCompiled {
                    operation_revision,
                    compiled: prepared.authorized.plan.compiled.clone(),
                },
            })?;
            self.record(NewEvent {
                task_id,
                run_id: None,
                operation_id: Some(operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::SandboxLeaseIssued {
                    operation_revision,
                    lease_id: prepared.lease.lease.lease_id,
                    expires_at_ms: prepared.lease.lease.expires_at_ms,
                    profile_digest: prepared.lease.lease.profile_digest.clone(),
                    action_digest: prepared.lease.lease.action_digest.clone(),
                },
            })?;
            lock(&self.inner.sandbox_leases)?.issue(prepared.lease.clone())?;
            let mut authorizations = lock(&self.inner.authorized_sandboxes)?;
            if authorizations.contains_key(&operation_id) {
                return Err(DaemonError::SandboxAuthorizationAlreadyExists(operation_id));
            }
            authorizations.insert(operation_id, prepared.authorized.clone());
            Ok(())
        })();
        if activation.is_err() {
            cleanup_scratch_directory(&prepared.authorized.scratch_directory);
        }
        activation
    }

    pub fn begin_operation(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
    ) -> Result<OperationRecord, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if !matches!(
            record.action,
            OperationAction::Opaque { .. }
                | OperationAction::McpServerLaunch { .. }
                | OperationAction::McpToolCall { .. }
                | OperationAction::BrokeredMcpToolCall { .. }
        ) {
            return Err(DaemonError::GenericDispatchRequiresOpaqueAction);
        }
        if record.state != OperationState::Authorized {
            return Err(DaemonError::OperationNotAuthorized(record.state));
        }
        self.transition(
            task_id,
            operation_id,
            expected_revision,
            OperationState::Dispatching,
            Actor::System,
            Some("brokered executor accepted the authorized operation".into()),
        )
    }

    pub fn complete_operation(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        completion: OperationCompletion,
    ) -> Result<OperationRecord, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if !matches!(
            record.action,
            OperationAction::Opaque { .. }
                | OperationAction::McpServerLaunch { .. }
                | OperationAction::McpToolCall { .. }
                | OperationAction::BrokeredMcpToolCall { .. }
        ) {
            return Err(DaemonError::GenericDispatchRequiresOpaqueAction);
        }
        let outcome = completion.outcome.unwrap_or(if completion.succeeded {
            OperationOutcome::Succeeded
        } else {
            OperationOutcome::Failed
        });
        if completion.outcome.is_some() && completion.succeeded != outcome.succeeded() {
            return Err(DaemonError::InconsistentOperationOutcome);
        }
        let target_state = match outcome {
            OperationOutcome::Succeeded => OperationState::Succeeded,
            OperationOutcome::Failed => OperationState::Failed,
            OperationOutcome::UnknownExecution => OperationState::UnknownExecution,
        };
        let resolves_unknown = record.state == OperationState::UnknownExecution
            && target_state != OperationState::UnknownExecution;
        if record.state != OperationState::Dispatching && !resolves_unknown {
            return Err(DaemonError::OperationNotDispatching(record.state));
        }
        let executor = bounded_nonempty(completion.executor, 256, "executor")?;
        let summary = bounded_nonempty(completion.summary, 16 * 1024, "operation receipt summary")?;
        if completion
            .result_digest
            .as_ref()
            .is_some_and(|digest| !is_sha256(digest))
        {
            return Err(DaemonError::InvalidResultDigest);
        }
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::OperationReceipt {
                operation_revision: expected_revision,
                executor,
                succeeded: outcome.succeeded(),
                outcome: Some(outcome),
                summary: summary.clone(),
                result_digest: completion.result_digest,
            },
        })?;
        self.transition(
            task_id,
            operation_id,
            expected_revision,
            target_state,
            Actor::System,
            Some(summary),
        )
    }

    pub fn accept_genui_artifact(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        candidate: GenUiArtifactCandidate,
    ) -> Result<AcceptedGenUiArtifact, DaemonError> {
        self.accept_genui_artifact_inner(task_id, operation_id, expected_revision, None, candidate)
    }

    pub(crate) fn accept_genui_artifact_from_base(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        base_artifact_id: hyper_term_protocol::ArtifactId,
        base_source_revision: u64,
        candidate: GenUiArtifactCandidate,
    ) -> Result<AcceptedGenUiArtifact, DaemonError> {
        self.accept_genui_artifact_inner(
            task_id,
            operation_id,
            expected_revision,
            Some((base_artifact_id, base_source_revision)),
            candidate,
        )
    }

    fn accept_genui_artifact_inner(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        required_base: Option<(hyper_term_protocol::ArtifactId, u64)>,
        candidate: GenUiArtifactCandidate,
    ) -> Result<AcceptedGenUiArtifact, DaemonError> {
        let _acceptance = lock(&self.inner.artifact_acceptance)?;
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if record.state != OperationState::Dispatching {
            return Err(DaemonError::OperationNotDispatching(record.state));
        }
        if record.kind != OperationKind::McpTool
            || !matches!(
                &record.action,
                OperationAction::BrokeredMcpToolCall { call }
                    if call.tool_name == "hyper_term.genui.compile"
            ) && !matches!(
                &record.action,
                OperationAction::Opaque { kind, .. } if kind == "hyper_term.genui.compile"
            )
        {
            return Err(DaemonError::ArtifactAcceptanceRequiresGenUiCompile);
        }
        if let Some((base_artifact_id, base_source_revision)) = required_base {
            let current = self
                .active_genui_artifact(task_id)?
                .filter(|artifact| {
                    artifact.artifact_id == base_artifact_id
                        && artifact.source_revision == base_source_revision
                })
                .ok_or(DaemonError::ArtifactBaseNotCurrent {
                    artifact_id: base_artifact_id,
                    source_revision: base_source_revision,
                })?;
            debug_assert_eq!(current.artifact_id, base_artifact_id);
        }
        let accepted = self.inner.artifacts.persist(candidate)?;
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::ArtifactAccepted {
                artifact: accepted.clone(),
            },
        })?;
        Ok(accepted)
    }

    pub(crate) fn active_genui_artifact(
        &self,
        task_id: TaskId,
    ) -> Result<Option<AcceptedGenUiArtifact>, DaemonError> {
        self.require_task(task_id)?;
        let authority = lock(&self.inner.authority)?;
        Ok(authority.journal.all().iter().rev().find_map(|event| {
            if event.task_id != task_id {
                return None;
            }
            match &event.payload {
                DomainEvent::ArtifactAccepted { artifact } => Some(artifact.clone()),
                _ => None,
            }
        }))
    }

    pub(crate) fn read_active_genui_artifact(
        &self,
        task_id: TaskId,
        artifact_id: hyper_term_protocol::ArtifactId,
    ) -> Result<StoredGenUiArtifact, DaemonError> {
        let accepted = self
            .active_genui_artifact(task_id)?
            .filter(|artifact| artifact.artifact_id == artifact_id)
            .ok_or(DaemonError::ArtifactNotActive(artifact_id))?;
        self.inner.artifacts.read(&accepted).map_err(Into::into)
    }

    pub(crate) fn genui_artifact_history(
        &self,
        task_id: TaskId,
        expected_active_artifact_id: hyper_term_protocol::ArtifactId,
    ) -> Result<Vec<GenUiArtifactHistoryEntry>, DaemonError> {
        self.require_task(task_id)?;
        let authority = lock(&self.inner.authority)?;
        let history = authority
            .journal
            .all()
            .iter()
            .rev()
            .filter_map(|event| {
                if event.task_id != task_id {
                    return None;
                }
                let DomainEvent::ArtifactAccepted { artifact } = &event.payload else {
                    return None;
                };
                Some(GenUiArtifactHistoryEntry {
                    event_sequence: event.sequence,
                    recorded_at_ms: event.recorded_at_ms,
                    operation_id: event.operation_id,
                    artifact: artifact.clone(),
                })
            })
            .take(MAX_GENUI_ARTIFACT_HISTORY)
            .collect::<Vec<_>>();
        if history.first().map(|entry| entry.artifact.artifact_id)
            != Some(expected_active_artifact_id)
        {
            return Err(DaemonError::ArtifactNotActive(expected_active_artifact_id));
        }
        Ok(history)
    }

    pub(crate) fn read_genui_artifact_revision(
        &self,
        task_id: TaskId,
        expected_active_artifact_id: hyper_term_protocol::ArtifactId,
        artifact_id: hyper_term_protocol::ArtifactId,
    ) -> Result<StoredGenUiArtifact, DaemonError> {
        self.require_task(task_id)?;
        let accepted = {
            let authority = lock(&self.inner.authority)?;
            let mut active_artifact_id = None;
            let mut accepted = None;
            for event in authority.journal.all().iter().rev() {
                if event.task_id != task_id {
                    continue;
                }
                let DomainEvent::ArtifactAccepted { artifact } = &event.payload else {
                    continue;
                };
                active_artifact_id.get_or_insert(artifact.artifact_id);
                if artifact.artifact_id == artifact_id {
                    accepted = Some(artifact.clone());
                }
                if active_artifact_id.is_some() && accepted.is_some() {
                    break;
                }
            }
            if active_artifact_id != Some(expected_active_artifact_id) {
                return Err(DaemonError::ArtifactNotActive(expected_active_artifact_id));
            }
            accepted.ok_or(DaemonError::ArtifactNotInHistory(artifact_id))?
        };
        self.inner.artifacts.read(&accepted).map_err(Into::into)
    }
}

#[derive(Default)]
struct OutputObservation {
    last_sequence: u64,
    pending_bytes: u64,
    recorded_bytes: u64,
}

impl OutputObservation {
    fn observe(&mut self, sequence: u64, bytes: u64) {
        if sequence <= self.last_sequence {
            return;
        }
        self.last_sequence = sequence;
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
    }

    fn observe_snapshot(&mut self, snapshot: &hyper_term_core::TerminalSnapshot) {
        self.last_sequence = self
            .last_sequence
            .max(snapshot.next_sequence.saturating_sub(1));
        let accounted = self.recorded_bytes.saturating_add(self.pending_bytes);
        self.pending_bytes = self
            .pending_bytes
            .saturating_add(snapshot.total_bytes.saturating_sub(accounted));
    }
}

fn now_ms() -> Result<u64, DaemonError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DaemonError::ClockBeforeUnixEpoch)?
        .as_millis();
    u64::try_from(milliseconds).map_err(|_| DaemonError::ClockOutOfRange)
}

fn create_private_directory(path: &Path) -> Result<(), DaemonError> {
    private_fs::ensure_private_directory(path)?;
    Ok(())
}

fn cleanup_scratch_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, DaemonError> {
    mutex.lock().map_err(|_| DaemonError::LockPoisoned)
}

#[cfg(unix)]
mod unix_server;

#[cfg(unix)]
pub use unix_server::{
    UnixServerHandle, run_unix_server, spawn_agent_capability_server,
    spawn_agent_capability_server_with_policy, spawn_unix_server,
};

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("daemon I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    Projector(#[from] ProjectorError),
    #[error(transparent)]
    Terminal(#[from] TerminalError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    ExecutionAuthorization(#[from] ExecutionAuthorizationError),
    #[error(transparent)]
    SandboxLease(#[from] SandboxLeaseError),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error("artifact store rejected the candidate: {0}")]
    ArtifactStore(String),
    #[error("daemon state lock is poisoned")]
    LockPoisoned,
    #[error("daemon state directory is already owned by another process: {0}")]
    StateDirectoryInUse(PathBuf),
    #[error("daemon state lock is not a regular private file: {0}")]
    InvalidStateLock(PathBuf),
    #[error("task title must not be empty")]
    EmptyTaskTitle,
    #[error("message text must not be empty")]
    EmptyMessage,
    #[error("message text contains {0} bytes and exceeds the 65536-byte event bound")]
    MessageTooLarge(usize),
    #[error("external message id exceeds the 4096-byte bound")]
    ExternalMessageIdTooLarge,
    #[error("invalid Agent projection: {0}")]
    InvalidAgentProjection(String),
    #[error("task {0} does not exist")]
    TaskNotFound(TaskId),
    #[error("task {0} appears more than once in the journal")]
    DuplicateTask(TaskId),
    #[error("journal contains an event before task {0} was created")]
    JournalEventBeforeTask(TaskId),
    #[error("operation {0} does not exist")]
    OperationNotFound(OperationId),
    #[error("operation belongs to task {expected}, not {actual}")]
    OperationTaskMismatch { expected: TaskId, actual: TaskId },
    #[error("stale operation revision: expected {expected}, got {actual}")]
    StaleOperationRevision { expected: u64, actual: u64 },
    #[error("operation is {0:?}, not waiting for permission")]
    OperationNotWaiting(OperationState),
    #[error("the operation could not produce bounded approval detail")]
    InvalidApprovalDetail,
    #[error("the approval detail no longer matches the reviewed operation")]
    ApprovalDetailMismatch,
    #[error("operation is {0:?}, not authorized")]
    OperationNotAuthorized(OperationState),
    #[error("operation is {0:?}, not dispatching")]
    OperationNotDispatching(OperationState),
    #[error("generic dispatch accepts only opaque or typed local MCP actions")]
    GenericDispatchRequiresOpaqueAction,
    #[error("artifact acceptance requires the dispatching hyper_term.genui.compile operation")]
    ArtifactAcceptanceRequiresGenUiCompile,
    #[error("artifact {0} is not the task's current accepted artifact")]
    ArtifactNotActive(hyper_term_protocol::ArtifactId),
    #[error("artifact {0} is not present in the task's accepted history")]
    ArtifactNotInHistory(hyper_term_protocol::ArtifactId),
    #[error(
        "artifact {artifact_id} revision {source_revision} is no longer the current draft base"
    )]
    ArtifactBaseNotCurrent {
        artifact_id: hyper_term_protocol::ArtifactId,
        source_revision: u64,
    },
    #[error("{label} must contain between 1 and {maximum} bytes")]
    InvalidBoundedText { label: &'static str, maximum: usize },
    #[error("operation result digest must be a lowercase SHA-256 value")]
    InvalidResultDigest,
    #[error("operation succeeded flag conflicts with its structured outcome")]
    InconsistentOperationOutcome,
    #[error("operation kind and action payload do not match")]
    ActionKindMismatch,
    #[error("operation environment binding {0} requires a broker-owned opaque channel")]
    UnsafeOperationEnvironment(String),
    #[error("local MCP server launch identity is invalid")]
    InvalidMcpServerLaunch,
    #[error("local MCP server runtime receipt is invalid")]
    InvalidLocalMcpRuntimeReceipt,
    #[error("local MCP tool call identity is invalid")]
    InvalidLocalMcpToolCall,
    #[error("local MCP tool call receipt is invalid")]
    InvalidLocalMcpToolCallReceipt,
    #[error("brokered MCP runtime failed: {0}")]
    BrokeredMcpRuntime(String),
    #[error("task {0} already has a brokered MCP runtime")]
    BrokeredMcpRuntimeAlreadyRegistered(TaskId),
    #[error("task {0} has no brokered MCP runtime")]
    BrokeredMcpRuntimeMissing(TaskId),
    #[error("brokered MCP proposal digest must be a lowercase SHA-256 value")]
    InvalidBrokeredMcpDigest,
    #[error("brokered MCP arguments contain {0} bytes and exceed the 1 MiB bound")]
    BrokeredMcpArgumentsTooLarge(usize),
    #[error("brokered MCP request does not match the dispatching operation")]
    BrokeredMcpBindingMismatch,
    #[error("brokered MCP operation was replayed with different inputs")]
    BrokeredMcpReplayMismatch,
    #[error("Agent MCP capability policy is invalid or unbounded")]
    InvalidAgentCapabilityPolicy,
    #[error("the negotiated local MCP runtime for this call is not recorded")]
    LocalMcpRuntimeNotRecorded,
    #[error("terminal dispatch only supports an exact shell action")]
    UnsupportedTerminalAction,
    #[error("sandboxed Agent commands require an explicit working directory")]
    SandboxWorkingDirectoryRequired,
    #[error("sandbox working directory {path} is invalid: {message}")]
    InvalidSandboxWorkingDirectory { path: PathBuf, message: String },
    #[error("Agent workspace may not be located inside Hyper Term daemon state")]
    WorkspaceInsideDaemonState,
    #[error("sandbox backend did not produce an enforced operating-system boundary")]
    UnenforcedSandboxBackend,
    #[error("Tier 2 dispatch requires the sandbox.isolated_task capability")]
    IsolatedTaskCapabilityRequired,
    #[error("Tier 2 runner exceeds the limits approved by the permission broker")]
    IsolatedRunnerPolicyMismatch,
    #[error("Tier 2 operations must use VM dispatch instead of the host PTY")]
    IsolatedTaskRequiresVmDispatch,
    #[error("operation {0} already has a retained Tier 2 result")]
    IsolatedResultAlreadyExists(OperationId),
    #[error("operation {0} has no retained Tier 2 result")]
    IsolatedResultMissing(OperationId),
    #[error("operation {0} has a pending Tier 2 acceptance review")]
    IsolatedResultHasPendingAcceptance(OperationId),
    #[error("Tier 2 result store failed integrity validation")]
    InvalidIsolatedResultStore,
    #[error("Tier 2 result path is not a safe reviewed relative path")]
    InvalidIsolatedResultPath,
    #[error("Tier 2 result content no longer matches its reviewed digest")]
    IsolatedResultDigestMismatch,
    #[error("Tier 2 result contains type changes or unsupported bounds")]
    UnsupportedIsolatedAcceptance,
    #[error("operation {0} has no prepared Tier 2 acceptance review")]
    IsolatedAcceptanceMissing(OperationId),
    #[error("operation {0} already has a pending Tier 2 acceptance review")]
    IsolatedAcceptanceAlreadyExists(OperationId),
    #[error("Tier 2 acceptance review store failed integrity validation")]
    InvalidIsolatedAcceptanceStore,
    #[error("Tier 2 acceptance no longer matches its reviewed result and workspace base")]
    IsolatedAcceptanceMismatch,
    #[error("operation {0} has no live one-use sandbox authorization")]
    SandboxAuthorizationMissing(OperationId),
    #[error("operation {0} already has a live sandbox authorization")]
    SandboxAuthorizationAlreadyExists(OperationId),
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("system clock is outside the supported millisecond range")]
    ClockOutOfRange,
    #[error("terminal {0} has no active daemon context")]
    TerminalContextMissing(TerminalId),
    #[error("terminal {0} already has an input lease")]
    InputLeaseHeld(TerminalId),
    #[error("terminal {0} has no input lease")]
    InputLeaseMissing(TerminalId),
    #[error("terminal {0} input lease does not match this client")]
    InputLeaseMismatch(TerminalId),
    #[error("request client identity does not match the connection handshake")]
    ClientIdentityMismatch,
    #[error("the first client frame must be Hello")]
    HelloRequired,
    #[error("Hello may only be sent once per connection")]
    DuplicateHello,
    #[error("refusing to replace non-socket path {0}")]
    UnsafeSocketPath(PathBuf),
    #[error("daemon socket is already in use: {0}")]
    SocketInUse(PathBuf),
    #[error(transparent)]
    IsolatedWorktree(#[from] IsolatedWorktreeError),
    #[error(transparent)]
    LimaRunner(#[from] LimaRunnerError),
    #[error("workspace acceptance failed: {0}")]
    WorkspaceApply(String),
}

impl DaemonError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TaskNotFound(_) => "task_not_found",
            Self::OperationNotFound(_) => "operation_not_found",
            Self::OperationTaskMismatch { .. } => "operation_task_mismatch",
            Self::StaleOperationRevision { .. } => "stale_operation_revision",
            Self::OperationNotWaiting(_) => "operation_not_waiting",
            Self::OperationNotAuthorized(_) => "operation_not_authorized",
            Self::OperationNotDispatching(_) => "operation_not_dispatching",
            Self::ActionKindMismatch
            | Self::InvalidMcpServerLaunch
            | Self::InvalidLocalMcpToolCall
            | Self::UnsupportedTerminalAction
            | Self::GenericDispatchRequiresOpaqueAction
            | Self::ArtifactAcceptanceRequiresGenUiCompile => "unsupported_action",
            Self::ArtifactNotActive(_) => "artifact_not_active",
            Self::ArtifactNotInHistory(_) => "artifact_not_in_history",
            Self::ArtifactBaseNotCurrent { .. } => "artifact_base_not_current",
            Self::ArtifactStore(_) => "artifact_error",
            Self::SandboxWorkingDirectoryRequired
            | Self::InvalidSandboxWorkingDirectory { .. }
            | Self::WorkspaceInsideDaemonState => "invalid_sandbox_workspace",
            Self::SandboxAuthorizationMissing(_)
            | Self::SandboxAuthorizationAlreadyExists(_)
            | Self::UnenforcedSandboxBackend
            | Self::IsolatedTaskCapabilityRequired
            | Self::IsolatedRunnerPolicyMismatch
            | Self::IsolatedTaskRequiresVmDispatch
            | Self::IsolatedResultAlreadyExists(_)
            | Self::IsolatedResultMissing(_)
            | Self::IsolatedResultHasPendingAcceptance(_)
            | Self::InvalidIsolatedResultStore
            | Self::InvalidIsolatedResultPath
            | Self::IsolatedResultDigestMismatch
            | Self::UnsupportedIsolatedAcceptance
            | Self::IsolatedAcceptanceMissing(_)
            | Self::IsolatedAcceptanceAlreadyExists(_)
            | Self::InvalidIsolatedAcceptanceStore
            | Self::IsolatedAcceptanceMismatch
            | Self::IsolatedWorktree(_)
            | Self::LimaRunner(_)
            | Self::WorkspaceApply(_)
            | Self::Sandbox(_)
            | Self::SandboxLease(_) => "sandbox_error",
            Self::InvalidBoundedText { .. }
            | Self::InvalidResultDigest
            | Self::InconsistentOperationOutcome
            | Self::InvalidBrokeredMcpDigest
            | Self::BrokeredMcpArgumentsTooLarge(_)
            | Self::BrokeredMcpBindingMismatch
            | Self::BrokeredMcpReplayMismatch
            | Self::InvalidAgentCapabilityPolicy
            | Self::InvalidLocalMcpRuntimeReceipt
            | Self::InvalidLocalMcpToolCallReceipt
            | Self::LocalMcpRuntimeNotRecorded
            | Self::UnsafeOperationEnvironment(_)
            | Self::EmptyMessage
            | Self::MessageTooLarge(_)
            | Self::ExternalMessageIdTooLarge
            | Self::InvalidAgentProjection(_) => "invalid_request",
            Self::InputLeaseHeld(_) => "input_lease_held",
            Self::InputLeaseMissing(_) | Self::InputLeaseMismatch(_) => "input_lease_mismatch",
            Self::ClientIdentityMismatch => "client_identity_mismatch",
            Self::HelloRequired | Self::DuplicateHello => "protocol_error",
            Self::UnsafeSocketPath(_) | Self::SocketInUse(_) => "socket_error",
            _ => "daemon_error",
        }
    }
}

impl From<ArtifactStoreError> for DaemonError {
    fn from(error: ArtifactStoreError) -> Self {
        Self::ArtifactStore(error.to_string())
    }
}
