//! Out-of-process authority host for Hyper Term.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use hyper_term_core::{
    BlockProjector, CapabilityLeaseLedger, JournalError, JsonlJournal, OperationError,
    OperationRecord, OperationReducer, ProjectorError, SandboxCompileRequest, SandboxError,
    SandboxLaunchPlan, SandboxLauncher, SandboxLeaseError, SandboxLeaseExpectation, TerminalConfig,
    TerminalError, TerminalEvent, TerminalReplay, TerminalSessionHandle, TerminalSubscription,
    TerminalSupervisor, UserShellConfig,
};
use hyper_term_protocol::{
    AcceptedGenUiArtifact, Actor, AgentExecutionContextReceiptSet, BlockDocument, BlockId,
    BlockPatch, BrokeredMcpToolExecution, CapabilityLease, ClientId, CompiledSandboxProfile,
    ContextReceipt, ControlRequest, ControlRequestEnvelope, ControlResponse,
    ControlResponseEnvelope, DomainEvent, EXECUTION_CONTEXT_SCHEMA_VERSION, EventEnvelope,
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
    IsolatedTaskReceipt, IsolatedTaskRequest, IsolatedTaskTermination, IsolatedWorktree,
    IsolatedWorktreeError, IsolatedWorktreeManager, IsolatedWorktreeRequest,
    LimaIsolatedTaskLauncher, LimaRunnerError, LimaTaskRunner, MacOsSeatbeltLauncher,
    cleanup_interrupted_lima_environment, read_isolated_task_receipt,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

mod agent_gateway;
mod artifact_debug_capsule;
mod artifact_editor_store;
mod artifact_runtime_trace_store;
mod artifact_store;
#[cfg(unix)]
mod client;
mod editor_lsp;
mod local_mcp_runtime;
#[cfg(unix)]
mod mcp_gateway;
mod network_proxy;
mod web_gateway;
mod workspace_apply;
mod workspace_diff;
mod workspace_snapshot;

use artifact_store::{ArtifactStore, ArtifactStoreError, StoredGenUiArtifact};
use workspace_apply::{
    DurableWorkspaceApplyResult, WorkspaceApplyRequest, WorkspaceApplySetPlan,
    WorkspaceTransactionContext, WorkspaceTransactionOutcome, acknowledge_workspace_transaction,
    apply_workspace_set_plan_durable, prepare_workspace_apply_requests,
    validate_workspace_apply_set,
};

pub use artifact_debug_capsule::{BugCapsuleError, load_bug_capsule};

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
    DesktopSessionSnapshot, DesktopWorkspaceSnapshot, DesktopWorkspaceStore, TerminalGatewayConfig,
    TerminalGatewayError, TerminalGatewayHandle, spawn_terminal_gateway,
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
    plan: SandboxLaunchPlan,
    scratch_directory: PathBuf,
}

struct PreparedSandbox {
    authorized: AuthorizedSandbox,
    lease: CapabilityLease,
}

#[derive(Clone)]
struct SandboxExecutionContext {
    compiled: CompiledSandboxProfile,
    started_at_ms: u64,
    scratch_directory: PathBuf,
}

#[derive(Clone)]
struct IsolatedResult {
    environment: IsolatedWorktree,
    scratch_directory: PathBuf,
    receipt: IsolatedTaskReceipt,
}

#[derive(Clone)]
struct IsolatedAcceptance {
    source_operation_id: OperationId,
    workspace: PathBuf,
    plan: WorkspaceApplySetPlan,
    binding_digest: String,
}

struct PreparedIsolatedAcceptance {
    workspace: PathBuf,
    plan: WorkspaceApplySetPlan,
    binding_digest: String,
}

#[cfg(unix)]
#[derive(Clone)]
struct CachedBrokeredMcpExecution {
    task_id: TaskId,
    operation_revision: u64,
    tool_name: String,
    proposal_digest: String,
    arguments_digest: String,
    execution: BrokeredMcpToolExecution,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredIsolatedAcceptance {
    schema_version: u32,
    acceptance_operation_id: OperationId,
    task_id: TaskId,
    source_operation_id: OperationId,
    workspace: PathBuf,
    plan: WorkspaceApplySetPlan,
    binding_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedAcceptanceReview {
    pub operation: OperationRecord,
    pub source_operation_id: OperationId,
    pub result_digest: String,
    pub target_paths: Vec<String>,
    pub changes: Vec<IsolatedAcceptanceChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedAcceptancePreview {
    pub source_operation_id: OperationId,
    pub result_digest: String,
    pub target_paths: Vec<String>,
    pub changes: Vec<IsolatedAcceptanceChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedAcceptanceChange {
    pub target_path: String,
    pub base_digest: Option<String>,
    pub proposed_digest: String,
    pub deleted: bool,
    pub binary: bool,
    pub base_bytes: u64,
    pub proposed_bytes: u64,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedResultReview {
    pub operation_id: OperationId,
    pub receipt: IsolatedTaskReceipt,
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
        fs::create_dir_all(state_directory.as_ref())?;
        let state_directory = fs::canonicalize(state_directory.as_ref())?;
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

    #[cfg(unix)]
    pub fn register_brokered_mcp_runtime(
        &self,
        task_id: TaskId,
        config: BrokeredMcpRuntimeConfig,
    ) -> Result<(), DaemonError> {
        self.require_task(task_id)?;
        let executor = mcp_gateway::BrokeredMcpExecutor::new(config)
            .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?;
        let mut runtimes = lock(&self.inner.brokered_mcp_runtimes)?;
        if runtimes.contains_key(&task_id) {
            return Err(DaemonError::BrokeredMcpRuntimeAlreadyRegistered(task_id));
        }
        runtimes.insert(task_id, Arc::new(Mutex::new(executor)));
        Ok(())
    }

    #[cfg(unix)]
    pub fn unregister_brokered_mcp_runtime(&self, task_id: TaskId) -> Result<(), DaemonError> {
        let executor = lock(&self.inner.brokered_mcp_runtimes)?.remove(&task_id);
        if let Some(executor) = executor {
            // Session shutdown must not delete the private runtime root while
            // an already-authorized Deno call is still using it.
            drop(lock(&executor)?);
        }
        lock(&self.inner.brokered_mcp_executions)?.retain(|_, cached| cached.task_id != task_id);
        Ok(())
    }

    #[cfg(unix)]
    pub fn execute_brokered_mcp_tool(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        tool_name: String,
        proposal_digest: String,
        arguments: serde_json::Value,
    ) -> Result<BrokeredMcpToolExecution, DaemonError> {
        let tool_name = bounded_nonempty(tool_name, 256, "brokered MCP tool name")?;
        if !is_sha256(&proposal_digest) {
            return Err(DaemonError::InvalidBrokeredMcpDigest);
        }
        let arguments_bytes = serde_json::to_vec(&arguments)
            .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?;
        if arguments_bytes.len() > 1024 * 1024 {
            return Err(DaemonError::BrokeredMcpArgumentsTooLarge(
                arguments_bytes.len(),
            ));
        }
        let recomputed_proposal_digest = Sha256::digest(
            serde_json::to_vec(&serde_json::json!({
                "name": tool_name,
                "arguments": arguments,
            }))
            .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?,
        )
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
        if recomputed_proposal_digest != proposal_digest {
            return Err(DaemonError::BrokeredMcpBindingMismatch);
        }
        let arguments_digest = Sha256::digest(&arguments_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if record.state != OperationState::Dispatching {
            return Err(DaemonError::OperationNotDispatching(record.state));
        }
        match &record.action {
            OperationAction::Opaque {
                kind,
                payload_digest,
            } if kind == &tool_name && payload_digest == &proposal_digest => {}
            _ => return Err(DaemonError::BrokeredMcpBindingMismatch),
        }
        let cached_matches = |cached: &CachedBrokeredMcpExecution| {
            cached.task_id == task_id
                && cached.operation_revision == expected_revision
                && cached.tool_name == tool_name
                && cached.proposal_digest == proposal_digest
                && cached.arguments_digest == arguments_digest
        };
        if let Some(cached) = lock(&self.inner.brokered_mcp_executions)?
            .get(&operation_id)
            .cloned()
        {
            return if cached_matches(&cached) {
                Ok(cached.execution)
            } else {
                Err(DaemonError::BrokeredMcpReplayMismatch)
            };
        }
        let executor = lock(&self.inner.brokered_mcp_runtimes)?
            .get(&task_id)
            .cloned()
            .ok_or(DaemonError::BrokeredMcpRuntimeMissing(task_id))?;
        let mut executor = lock(&executor)?;
        if let Some(cached) = lock(&self.inner.brokered_mcp_executions)?
            .get(&operation_id)
            .cloned()
        {
            return if cached_matches(&cached) {
                Ok(cached.execution)
            } else {
                Err(DaemonError::BrokeredMcpReplayMismatch)
            };
        }
        let execution = executor.execute(&tool_name, &arguments);
        lock(&self.inner.brokered_mcp_executions)?.insert(
            operation_id,
            CachedBrokeredMcpExecution {
                task_id,
                operation_revision: expected_revision,
                tool_name,
                proposal_digest,
                arguments_digest,
                execution: execution.clone(),
            },
        );
        Ok(execution)
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
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::PermissionRequested {
                operation_revision: waiting.revision,
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
                environment: SandboxEnvironmentPolicy {
                    clear_inherited: true,
                    variables: std::collections::BTreeMap::from([
                        (
                            "HOME".into(),
                            scratch_directory.to_string_lossy().into_owned(),
                        ),
                        (
                            "TMPDIR".into(),
                            scratch_directory.to_string_lossy().into_owned(),
                        ),
                        ("LANG".into(), "C.UTF-8".into()),
                        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
                        ("TERM".into(), "xterm-256color".into()),
                    ]),
                },
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
            let compile_request = SandboxCompileRequest {
                operation_id: record.operation_id,
                operation_revision: authorized_revision,
                actor: actor.clone(),
                command: normalized_command,
                profile,
            };
            let plan = if isolated_task {
                LimaIsolatedTaskLauncher.compile(&compile_request)?
            } else {
                self.inner.sandbox_launcher.compile(&compile_request)?
            };
            if !plan.compiled.enforced
                || plan.compiled.backend
                    == hyper_term_protocol::SandboxBackendKind::TestOnlyUnenforced
            {
                return Err(DaemonError::UnenforcedSandboxBackend);
            }
            let issued_at_ms = now_ms()?;
            let lease = CapabilityLease {
                lease_id: SandboxLeaseId::new(),
                operation_id: record.operation_id,
                operation_revision: authorized_revision,
                action_digest: plan.compiled.action_digest.clone(),
                profile_digest: plan.compiled.profile_digest.clone(),
                actor,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(SANDBOX_LEASE_TTL_MS),
                one_use: true,
            };
            Ok(PreparedSandbox {
                authorized: AuthorizedSandbox {
                    lease_id: lease.lease_id,
                    plan,
                    scratch_directory: scratch_directory.clone(),
                },
                lease,
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
        let operation_id = prepared.lease.operation_id;
        let activation = (|| {
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
                    lease_id: prepared.lease.lease_id,
                    expires_at_ms: prepared.lease.expires_at_ms,
                    profile_digest: prepared.lease.profile_digest.clone(),
                    action_digest: prepared.lease.action_digest.clone(),
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

    /// Runs an explicitly approved shell operation in a Tier 2 VM and retains
    /// its exact-commit result for later review or discard. This method never
    /// applies changes to the user's workspace.
    pub fn dispatch_isolated_task(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        runner: &LimaTaskRunner,
        cancelled: &AtomicBool,
    ) -> Result<IsolatedTaskReceipt, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if record.state != OperationState::Authorized {
            return Err(DaemonError::OperationNotAuthorized(record.state));
        }
        if !record
            .required_capabilities
            .iter()
            .any(|capability| capability == ISOLATED_TASK_CAPABILITY)
        {
            return Err(DaemonError::IsolatedTaskCapabilityRequired);
        }
        if runner.task_timeout().as_millis() > ISOLATED_TASK_WALL_TIME_MS as u128
            || runner.max_output_bytes() as u64 > ISOLATED_TASK_MAX_OUTPUT_BYTES
        {
            return Err(DaemonError::IsolatedRunnerPolicyMismatch);
        }
        if lock(&self.inner.isolated_results)?.contains_key(&operation_id) {
            return Err(DaemonError::IsolatedResultAlreadyExists(operation_id));
        }
        let OperationAction::Shell { .. } = record.action else {
            return Err(DaemonError::UnsupportedTerminalAction);
        };
        let authorized = self.consume_authorized_sandbox(&record)?;
        if authorized.plan.compiled.backend != hyper_term_protocol::SandboxBackendKind::LimaVm
            || authorized.plan.compiled.profile.enforcement != SandboxEnforcement::IsolatedTask
        {
            cleanup_scratch_directory(&authorized.scratch_directory);
            return Err(DaemonError::IsolatedRunnerPolicyMismatch);
        }
        if let Err(error) = self.transition(
            task_id,
            operation_id,
            expected_revision,
            OperationState::Dispatching,
            Actor::System,
            Some("one-use Tier 2 lease consumed before VM materialization".into()),
        ) {
            cleanup_scratch_directory(&authorized.scratch_directory);
            return Err(error);
        }
        let started_at_ms = now_ms()?;
        let command = authorized.plan.command.clone();
        let workspace = command
            .cwd
            .as_deref()
            .ok_or(DaemonError::SandboxWorkingDirectoryRequired)?;
        let environment =
            match self
                .inner
                .isolated_worktree_manager
                .create(&IsolatedWorktreeRequest {
                    source_workspace: workspace.to_path_buf(),
                    state_root: authorized.scratch_directory.clone(),
                    task_id: operation_id.to_string(),
                    revision: Some("HEAD".into()),
                }) {
                Ok(environment) => environment,
                Err(error) => {
                    cleanup_scratch_directory(&authorized.scratch_directory);
                    let _ = self.transition(
                        task_id,
                        operation_id,
                        expected_revision + 1,
                        OperationState::Failed,
                        Actor::System,
                        Some(format!("Tier 2 worktree materialization failed: {error}")),
                    );
                    return Err(error.into());
                }
            };
        let mut argv = Vec::with_capacity(command.env.len() + command.args.len() + 2);
        if !command.env.is_empty() {
            argv.push("/usr/bin/env".into());
            argv.extend(
                command
                    .env
                    .iter()
                    .map(|(name, value)| format!("{name}={value}")),
            );
        }
        argv.push(command.program.clone());
        argv.extend(command.args.clone());
        let receipt = match runner.run(
            &self.inner.isolated_worktree_manager,
            &environment,
            &IsolatedTaskRequest { argv },
            cancelled,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                let _ = self.inner.isolated_worktree_manager.destroy(&environment);
                cleanup_scratch_directory(&authorized.scratch_directory);
                let _ = self.record_sandbox_receipt(
                    task_id,
                    operation_id,
                    expected_revision + 1,
                    &authorized.plan.compiled,
                    started_at_ms,
                    now_ms().unwrap_or(started_at_ms),
                    SandboxOutcome::Unknown,
                    None,
                );
                let _ = self.transition(
                    task_id,
                    operation_id,
                    expected_revision + 1,
                    OperationState::Failed,
                    Actor::System,
                    Some(format!("Tier 2 execution failed closed: {error}")),
                );
                return Err(error.into());
            }
        };
        let (outcome, state) = match (&receipt.termination, receipt.exit_code) {
            (IsolatedTaskTermination::Exited, Some(0)) => {
                (SandboxOutcome::Succeeded, OperationState::Succeeded)
            }
            (IsolatedTaskTermination::Cancelled, _) => {
                (SandboxOutcome::Denied, OperationState::Cancelled)
            }
            (IsolatedTaskTermination::TimedOut | IsolatedTaskTermination::Signaled, _) => {
                (SandboxOutcome::Violated, OperationState::Violated)
            }
            (IsolatedTaskTermination::Exited, _) => {
                (SandboxOutcome::Failed, OperationState::Failed)
            }
        };
        let previous = lock(&self.inner.isolated_results)?.insert(
            operation_id,
            IsolatedResult {
                environment,
                scratch_directory: authorized.scratch_directory,
                receipt: receipt.clone(),
            },
        );
        debug_assert!(previous.is_none());
        self.record_sandbox_receipt(
            task_id,
            operation_id,
            expected_revision + 1,
            &authorized.plan.compiled,
            receipt.started_at_ms,
            receipt.finished_at_ms,
            outcome,
            receipt.exit_code.and_then(|code| u32::try_from(code).ok()),
        )?;
        self.transition(
            task_id,
            operation_id,
            expected_revision + 1,
            state,
            Actor::System,
            Some(format!(
                "Tier 2 result retained for review: {} changed files, inventory {}",
                receipt.changes.changed_files.len(),
                receipt.changes.inventory_sha256
            )),
        )?;
        Ok(receipt)
    }

    pub fn discard_isolated_result(&self, operation_id: OperationId) -> Result<(), DaemonError> {
        if lock(&self.inner.isolated_acceptances)?
            .values()
            .any(|acceptance| acceptance.source_operation_id == operation_id)
        {
            return Err(DaemonError::IsolatedResultHasPendingAcceptance(
                operation_id,
            ));
        }
        let result = lock(&self.inner.isolated_results)?
            .remove(&operation_id)
            .ok_or(DaemonError::IsolatedResultMissing(operation_id))?;
        self.inner
            .isolated_worktree_manager
            .destroy(&result.environment)?;
        cleanup_scratch_directory(&result.scratch_directory);
        Ok(())
    }

    pub fn isolated_result_receipt(
        &self,
        operation_id: OperationId,
    ) -> Result<IsolatedTaskReceipt, DaemonError> {
        Ok(lock(&self.inner.isolated_results)?
            .get(&operation_id)
            .ok_or(DaemonError::IsolatedResultMissing(operation_id))?
            .receipt
            .clone())
    }

    pub fn isolated_result_reviews(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<IsolatedResultReview>, DaemonError> {
        self.require_task(task_id)?;
        let retained = lock(&self.inner.isolated_results)?
            .iter()
            .map(|(operation_id, result)| (*operation_id, result.receipt.clone()))
            .collect::<Vec<_>>();
        let mut reviews = Vec::new();
        for (operation_id, receipt) in retained {
            if self.operation(operation_id)?.task_id == task_id {
                reviews.push(IsolatedResultReview {
                    operation_id,
                    receipt,
                });
            }
        }
        reviews.sort_by_key(|review| (review.receipt.finished_at_ms, review.operation_id));
        Ok(reviews)
    }

    pub fn read_isolated_result_file(
        &self,
        operation_id: OperationId,
        relative_path: &Path,
        expected_sha256: &str,
    ) -> Result<Vec<u8>, DaemonError> {
        if !safe_isolated_result_path(relative_path) || !is_sha256(expected_sha256) {
            return Err(DaemonError::InvalidIsolatedResultPath);
        }
        let results = lock(&self.inner.isolated_results)?;
        let result = results
            .get(&operation_id)
            .ok_or(DaemonError::IsolatedResultMissing(operation_id))?;
        let reviewed = result
            .receipt
            .changes
            .changed_files
            .iter()
            .find(|change| change.path == relative_path)
            .and_then(|change| {
                change
                    .content_sha256
                    .as_deref()
                    .filter(|digest| *digest == expected_sha256)
                    .map(|_| change)
            })
            .ok_or(DaemonError::IsolatedResultDigestMismatch)?;
        let target = result.environment.manifest.worktree.join(relative_path);
        let metadata = fs::symlink_metadata(&target)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != reviewed.bytes
            || metadata.len() > 8 * 1024 * 1024
        {
            return Err(DaemonError::IsolatedResultDigestMismatch);
        }
        let bytes = fs::read(target)?;
        let digest = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if digest != expected_sha256 {
            return Err(DaemonError::IsolatedResultDigestMismatch);
        }
        Ok(bytes)
    }

    pub fn propose_isolated_result_acceptance(
        &self,
        task_id: TaskId,
        source_operation_id: OperationId,
    ) -> Result<IsolatedAcceptanceReview, DaemonError> {
        let source_operation = self.operation(source_operation_id)?;
        if source_operation.task_id != task_id {
            return Err(DaemonError::OperationTaskMismatch {
                expected: source_operation.task_id,
                actual: task_id,
            });
        }
        if lock(&self.inner.isolated_acceptances)?
            .values()
            .any(|acceptance| acceptance.source_operation_id == source_operation_id)
        {
            return Err(DaemonError::IsolatedAcceptanceAlreadyExists(
                source_operation_id,
            ));
        }
        let prepared = self.prepare_isolated_result_acceptance(task_id, source_operation_id)?;
        let workspace = prepared.workspace;
        let plan = prepared.plan;
        let binding_digest = prepared.binding_digest;
        let target_paths = plan
            .plans
            .iter()
            .map(|plan| plan.target_path.clone())
            .collect::<Vec<_>>();
        let summary = isolated_acceptance_summary(source_operation_id, &target_paths);
        let operation = self.propose_operation(
            task_id,
            OperationKind::FileEdit,
            OperationAction::Opaque {
                kind: "hyper_term.tier2.accept".into(),
                payload_digest: binding_digest.clone(),
            },
            summary,
            RiskClass::WorkspaceWrite,
            vec!["workspace.write".into(), "sandbox.tier2.accept".into()],
        )?;
        let stored = StoredIsolatedAcceptance {
            schema_version: ISOLATED_ACCEPTANCE_SCHEMA_VERSION,
            acceptance_operation_id: operation.operation_id,
            task_id,
            source_operation_id,
            workspace: workspace.clone(),
            plan: plan.clone(),
            binding_digest: binding_digest.clone(),
        };
        if let Err(error) =
            write_isolated_acceptance(&self.inner.isolated_acceptances_root, &stored)
        {
            let _ = self.decide_permission(
                task_id,
                operation.operation_id,
                operation.revision,
                PermissionDecision::Cancelled,
            );
            return Err(error);
        }
        let previous = lock(&self.inner.isolated_acceptances)?.insert(
            operation.operation_id,
            IsolatedAcceptance {
                source_operation_id,
                workspace,
                plan: plan.clone(),
                binding_digest,
            },
        );
        debug_assert!(previous.is_none());
        self.isolated_acceptance_review(operation.operation_id)
    }

    pub fn preview_isolated_result_acceptance(
        &self,
        task_id: TaskId,
        source_operation_id: OperationId,
    ) -> Result<IsolatedAcceptancePreview, DaemonError> {
        let prepared = self.prepare_isolated_result_acceptance(task_id, source_operation_id)?;
        Ok(IsolatedAcceptancePreview {
            source_operation_id,
            result_digest: prepared.plan.result_digest.clone(),
            target_paths: prepared
                .plan
                .plans
                .iter()
                .map(|plan| plan.target_path.clone())
                .collect(),
            changes: isolated_acceptance_changes(&prepared.plan),
        })
    }

    fn prepare_isolated_result_acceptance(
        &self,
        task_id: TaskId,
        source_operation_id: OperationId,
    ) -> Result<PreparedIsolatedAcceptance, DaemonError> {
        let source_operation = self.operation(source_operation_id)?;
        if source_operation.task_id != task_id {
            return Err(DaemonError::OperationTaskMismatch {
                expected: source_operation.task_id,
                actual: task_id,
            });
        }
        let result = lock(&self.inner.isolated_results)?
            .get(&source_operation_id)
            .cloned()
            .ok_or(DaemonError::IsolatedResultMissing(source_operation_id))?;
        let mut requests = Vec::new();
        for change in &result.receipt.changes.changed_files {
            if change.kind == hyper_term_sandbox::IsolatedChangeKind::Deleted {
                requests.push(WorkspaceApplyRequest::Delete {
                    target_path: change.path.to_string_lossy().into_owned(),
                });
                continue;
            }
            if !matches!(
                change.kind,
                hyper_term_sandbox::IsolatedChangeKind::Added
                    | hyper_term_sandbox::IsolatedChangeKind::Modified
                    | hyper_term_sandbox::IsolatedChangeKind::Untracked
            ) {
                return Err(DaemonError::UnsupportedIsolatedAcceptance);
            }
            let digest = change
                .content_sha256
                .as_deref()
                .ok_or(DaemonError::IsolatedResultDigestMismatch)?;
            let bytes =
                self.read_isolated_result_file(source_operation_id, &change.path, digest)?;
            requests.push(WorkspaceApplyRequest::WriteBytes {
                target_path: change.path.to_string_lossy().into_owned(),
                proposed_bytes: bytes,
            });
        }
        let workspace = result.environment.manifest.source_workspace.clone();
        let plan = prepare_workspace_apply_requests(&workspace, requests)
            .map_err(|error| DaemonError::WorkspaceApply(error.to_string()))?;
        let binding_digest = isolated_acceptance_digest(
            source_operation_id,
            &result.receipt.changes.inventory_sha256,
            &workspace,
            &plan,
        )?;
        Ok(PreparedIsolatedAcceptance {
            workspace,
            plan,
            binding_digest,
        })
    }

    pub fn isolated_acceptance_review(
        &self,
        operation_id: OperationId,
    ) -> Result<IsolatedAcceptanceReview, DaemonError> {
        let acceptance = lock(&self.inner.isolated_acceptances)?
            .get(&operation_id)
            .cloned()
            .ok_or(DaemonError::IsolatedAcceptanceMissing(operation_id))?;
        let operation = self.operation(operation_id)?;
        Ok(IsolatedAcceptanceReview {
            operation,
            source_operation_id: acceptance.source_operation_id,
            result_digest: acceptance.plan.result_digest.clone(),
            target_paths: acceptance
                .plan
                .plans
                .iter()
                .map(|plan| plan.target_path.clone())
                .collect(),
            changes: isolated_acceptance_changes(&acceptance.plan),
        })
    }

    pub fn isolated_acceptance_reviews(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<IsolatedAcceptanceReview>, DaemonError> {
        self.require_task(task_id)?;
        let operation_ids = lock(&self.inner.isolated_acceptances)?
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut reviews = Vec::new();
        for operation_id in operation_ids {
            let review = self.isolated_acceptance_review(operation_id)?;
            if review.operation.task_id == task_id {
                reviews.push(review);
            }
        }
        reviews.sort_by_key(|review| review.operation.operation_id);
        Ok(reviews)
    }

    pub fn decide_isolated_acceptance_permission(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        decision: PermissionDecision,
    ) -> Result<OperationRecord, DaemonError> {
        if !lock(&self.inner.isolated_acceptances)?.contains_key(&operation_id) {
            return Err(DaemonError::IsolatedAcceptanceMissing(operation_id));
        }
        let updated = self.decide_permission(task_id, operation_id, expected_revision, decision)?;
        if matches!(updated.state, OperationState::Cancelled) {
            remove_isolated_acceptance(&self.inner.isolated_acceptances_root, operation_id)?;
            lock(&self.inner.isolated_acceptances)?.remove(&operation_id);
        }
        Ok(updated)
    }

    pub fn accept_isolated_result(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
    ) -> Result<OperationRecord, DaemonError> {
        let acceptance = lock(&self.inner.isolated_acceptances)?
            .get(&operation_id)
            .cloned()
            .ok_or(DaemonError::IsolatedAcceptanceMissing(operation_id))?;
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if !matches!(
            &record.action,
            OperationAction::Opaque { kind, payload_digest }
                if kind == "hyper_term.tier2.accept"
                    && payload_digest == &acceptance.binding_digest
        ) {
            return Err(DaemonError::IsolatedAcceptanceMismatch);
        }
        let source = self.isolated_result_receipt(acceptance.source_operation_id)?;
        if isolated_acceptance_digest(
            acceptance.source_operation_id,
            &source.changes.inventory_sha256,
            &acceptance.workspace,
            &acceptance.plan,
        )? != acceptance.binding_digest
        {
            return Err(DaemonError::IsolatedAcceptanceMismatch);
        }
        let dispatching = self.begin_operation(task_id, operation_id, expected_revision)?;
        let durable = apply_workspace_set_plan_durable(
            &acceptance.workspace,
            &self.inner.state_directory,
            WorkspaceTransactionContext {
                task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &acceptance.plan,
        );
        let receipt = match durable {
            Ok(DurableWorkspaceApplyResult::Committed(receipt))
            | Ok(DurableWorkspaceApplyResult::RolledBack(receipt)) => receipt,
            Err(error) => {
                if self
                    .complete_operation(
                        task_id,
                        operation_id,
                        dispatching.revision,
                        OperationCompletion {
                            executor: "hyper-term-tier2-accept".into(),
                            succeeded: false,
                            outcome: Some(OperationOutcome::UnknownExecution),
                            summary: error.to_string(),
                            result_digest: None,
                        },
                    )
                    .is_ok()
                {
                    let _ = remove_isolated_acceptance(
                        &self.inner.isolated_acceptances_root,
                        operation_id,
                    );
                    lock(&self.inner.isolated_acceptances)?.remove(&operation_id);
                }
                return Err(DaemonError::WorkspaceApply(error.to_string()));
            }
        };
        let committed = receipt.outcome == WorkspaceTransactionOutcome::Committed;
        let completed = self.complete_operation(
            task_id,
            operation_id,
            dispatching.revision,
            OperationCompletion {
                executor: "hyper-term-tier2-accept".into(),
                succeeded: committed,
                outcome: Some(if committed {
                    OperationOutcome::Succeeded
                } else {
                    OperationOutcome::Failed
                }),
                summary: if committed {
                    format!(
                        "applied {} reviewed Tier 2 file(s)",
                        acceptance.plan.plans.len()
                    )
                } else {
                    receipt
                        .failure_summary
                        .clone()
                        .unwrap_or_else(|| "Tier 2 acceptance rolled back".into())
                },
                result_digest: committed.then(|| receipt.result_digest.clone()),
            },
        )?;
        acknowledge_workspace_transaction(&self.inner.state_directory, receipt.transaction_id)
            .map_err(|error| DaemonError::WorkspaceApply(error.to_string()))?;
        remove_isolated_acceptance(&self.inner.isolated_acceptances_root, operation_id)?;
        lock(&self.inner.isolated_acceptances)?.remove(&operation_id);
        if committed {
            self.discard_isolated_result(acceptance.source_operation_id)?;
        }
        Ok(completed)
    }

    pub fn dispatch_terminal(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        size: TerminalSize,
    ) -> Result<TerminalId, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if record.state != OperationState::Authorized {
            return Err(DaemonError::OperationNotAuthorized(record.state));
        }
        let OperationAction::Shell { command } = record.action.clone() else {
            return Err(DaemonError::UnsupportedTerminalAction);
        };
        if record
            .required_capabilities
            .iter()
            .any(|capability| capability == ISOLATED_TASK_CAPABILITY)
        {
            return Err(DaemonError::IsolatedTaskRequiresVmDispatch);
        }

        let authorized = self.consume_authorized_sandbox(&record)?;
        let started_at_ms = now_ms()?;

        if let Err(error) = self.transition(
            task_id,
            operation_id,
            expected_revision,
            OperationState::Dispatching,
            Actor::System,
            Some("sandbox lease consumed before PTY spawn".into()),
        ) {
            cleanup_scratch_directory(&authorized.scratch_directory);
            return Err(error);
        }

        let session = match self.inner.terminals.spawn_sandboxed(
            &authorized.plan,
            &size,
            TerminalConfig::default(),
        ) {
            Ok(session) => session,
            Err(error) => {
                let message = error.to_string();
                let finished_at_ms = now_ms().unwrap_or(started_at_ms);
                let _ = self.record_sandbox_receipt(
                    task_id,
                    operation_id,
                    expected_revision + 1,
                    &authorized.plan.compiled,
                    started_at_ms,
                    finished_at_ms,
                    SandboxOutcome::Denied,
                    None,
                );
                let _ = self.transition(
                    task_id,
                    operation_id,
                    expected_revision + 1,
                    OperationState::Failed,
                    Actor::System,
                    Some(format!("PTY spawn failed: {message}")),
                );
                cleanup_scratch_directory(&authorized.scratch_directory);
                return Err(DaemonError::Terminal(error));
            }
        };
        let terminal_id = session.id();
        let subscription = session.subscribe(0);
        lock(&self.inner.terminal_contexts)?.insert(
            terminal_id,
            TerminalContext::Operation(OperationTerminalContext {
                task_id,
                operation_id,
            }),
        );
        lock(&self.inner.sandbox_executions)?.insert(
            terminal_id,
            SandboxExecutionContext {
                compiled: authorized.plan.compiled.clone(),
                started_at_ms,
                scratch_directory: authorized.scratch_directory.clone(),
            },
        );
        if let Err(error) = self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::TerminalOpened {
                terminal_id,
                command,
                size,
            },
        }) {
            let _ = session.close();
            lock(&self.inner.terminal_contexts)?.remove(&terminal_id);
            lock(&self.inner.sandbox_executions)?.remove(&terminal_id);
            let _ = self.record_sandbox_receipt(
                task_id,
                operation_id,
                expected_revision + 1,
                &authorized.plan.compiled,
                started_at_ms,
                now_ms().unwrap_or(started_at_ms),
                SandboxOutcome::Unknown,
                None,
            );
            let _ = self.transition(
                task_id,
                operation_id,
                expected_revision + 1,
                OperationState::Failed,
                Actor::System,
                Some("terminal-open event could not be journaled".into()),
            );
            cleanup_scratch_directory(&authorized.scratch_directory);
            return Err(error);
        }

        let daemon = self.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("hyperd-terminal-{terminal_id}"))
            .spawn(move || daemon.monitor_terminal(session, subscription, terminal_id))
        {
            let _ = self.inner.terminals.close(terminal_id);
            lock(&self.inner.terminal_contexts)?.remove(&terminal_id);
            lock(&self.inner.sandbox_executions)?.remove(&terminal_id);
            let _ = self.record(NewEvent {
                task_id,
                run_id: None,
                operation_id: Some(operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::TerminalExited {
                    terminal_id,
                    exit_code: None,
                },
            });
            let _ = self.record_sandbox_receipt(
                task_id,
                operation_id,
                expected_revision + 1,
                &authorized.plan.compiled,
                started_at_ms,
                now_ms().unwrap_or(started_at_ms),
                SandboxOutcome::Unknown,
                None,
            );
            let _ = self.transition(
                task_id,
                operation_id,
                expected_revision + 1,
                OperationState::Failed,
                Actor::System,
                Some("terminal monitor thread could not start".into()),
            );
            cleanup_scratch_directory(&authorized.scratch_directory);
            return Err(error.into());
        }
        Ok(terminal_id)
    }

    fn consume_authorized_sandbox(
        &self,
        record: &OperationRecord,
    ) -> Result<AuthorizedSandbox, DaemonError> {
        let authorized = lock(&self.inner.authorized_sandboxes)?
            .get(&record.operation_id)
            .cloned()
            .ok_or(DaemonError::SandboxAuthorizationMissing(
                record.operation_id,
            ))?;
        let expected = SandboxLeaseExpectation {
            operation_id: record.operation_id,
            operation_revision: record.revision,
            action_digest: authorized.plan.compiled.action_digest.clone(),
            profile_digest: authorized.plan.compiled.profile_digest.clone(),
            actor: Actor::System,
        };
        lock(&self.inner.sandbox_leases)?.consume(authorized.lease_id, &expected, now_ms()?)?;
        lock(&self.inner.authorized_sandboxes)?.remove(&record.operation_id);
        Ok(authorized)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_sandbox_receipt(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        operation_revision: u64,
        compiled: &CompiledSandboxProfile,
        started_at_ms: u64,
        finished_at_ms: u64,
        outcome: SandboxOutcome,
        exit_code: Option<u32>,
    ) -> Result<(), DaemonError> {
        let receipt = SandboxReceipt {
            backend: compiled.backend,
            enforced: compiled.enforced,
            profile_digest: compiled.profile_digest.clone(),
            action_digest: compiled.action_digest.clone(),
            started_at_ms,
            finished_at_ms,
            outcome,
            exit_code,
            violations: Vec::new(),
        };
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::SandboxReceiptRecorded {
                operation_revision,
                receipt: receipt.clone(),
            },
        })?;
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::OperationReceipt {
                operation_revision,
                executor: format!("sandbox::{:?}", compiled.backend),
                succeeded: outcome == SandboxOutcome::Succeeded,
                outcome: Some(match outcome {
                    SandboxOutcome::Succeeded => OperationOutcome::Succeeded,
                    SandboxOutcome::Unknown => OperationOutcome::UnknownExecution,
                    SandboxOutcome::Failed | SandboxOutcome::Violated | SandboxOutcome::Denied => {
                        OperationOutcome::Failed
                    }
                }),
                summary: format!(
                    "Agent command finished in an enforced {:?} sandbox with outcome {:?}",
                    compiled.backend, outcome
                ),
                result_digest: None,
            },
        })
    }

    /// Opens the user's configured login shell as a direct human terminal.
    /// The wire request cannot provide a program, arguments, or environment;
    /// those remain an authority-side decision and do not represent an AI
    /// operation requiring the effect permission pipeline.
    pub fn open_user_shell(
        &self,
        cwd: Option<PathBuf>,
        size: TerminalSize,
    ) -> Result<TerminalId, DaemonError> {
        let shell = UserShellConfig {
            cwd,
            ..UserShellConfig::default()
        };
        let session =
            self.inner
                .terminals
                .spawn_user_shell(&shell, &size, TerminalConfig::default())?;
        let terminal_id = session.id();
        let subscription = session.subscribe(0);
        lock(&self.inner.terminal_contexts)?.insert(terminal_id, TerminalContext::UserShell);

        let daemon = self.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("hyperd-user-shell-{terminal_id}"))
            .spawn(move || daemon.monitor_terminal(session, subscription, terminal_id))
        {
            let _ = self.inner.terminals.close(terminal_id);
            lock(&self.inner.terminal_contexts)?.remove(&terminal_id);
            return Err(error.into());
        }
        Ok(terminal_id)
    }

    pub fn terminal_subscription(
        &self,
        terminal_id: TerminalId,
        after_sequence: u64,
    ) -> Result<TerminalSubscription, DaemonError> {
        Ok(self
            .inner
            .terminals
            .get(terminal_id)?
            .subscribe(after_sequence))
    }

    pub fn resize_terminal(
        &self,
        terminal_id: TerminalId,
        generation: u64,
        size: TerminalSize,
    ) -> Result<(), DaemonError> {
        self.inner
            .terminals
            .get(terminal_id)?
            .resize(generation, &size)?;
        let context = self.terminal_context(terminal_id)?;
        if let TerminalContext::Operation(context) = context {
            self.record(NewEvent {
                task_id: context.task_id,
                run_id: None,
                operation_id: Some(context.operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::TerminalResized {
                    terminal_id,
                    generation,
                    size,
                },
            })?;
        }
        Ok(())
    }

    pub fn close_terminal(&self, terminal_id: TerminalId) -> Result<(), DaemonError> {
        let session = self.inner.terminals.get(terminal_id)?;
        if session.snapshot().exit.is_some() {
            return Ok(());
        }
        lock(&self.inner.cancelled_terminals)?.insert(terminal_id);
        session.close()?;
        Ok(())
    }

    pub fn acquire_input_lease(
        &self,
        terminal_id: TerminalId,
        client_id: ClientId,
    ) -> Result<(InputLeaseId, u64), DaemonError> {
        self.inner.terminals.get(terminal_id)?;
        let mut leases = lock(&self.inner.input_leases)?;
        if let Some(existing) = leases.get(&terminal_id) {
            if existing.client_id == client_id {
                return Ok((existing.lease_id, existing.generation));
            }
            return Err(DaemonError::InputLeaseHeld(terminal_id));
        }
        let generation = {
            let mut generations = lock(&self.inner.lease_generations)?;
            let generation = generations.entry(terminal_id).or_insert(0);
            *generation += 1;
            *generation
        };
        let lease_id = InputLeaseId::new();
        leases.insert(
            terminal_id,
            InputLease {
                lease_id,
                client_id,
                generation,
            },
        );
        Ok((lease_id, generation))
    }

    pub fn release_input_lease(
        &self,
        terminal_id: TerminalId,
        lease_id: InputLeaseId,
        client_id: ClientId,
    ) -> Result<(), DaemonError> {
        let mut leases = lock(&self.inner.input_leases)?;
        let existing = leases
            .get(&terminal_id)
            .ok_or(DaemonError::InputLeaseMissing(terminal_id))?;
        if existing.lease_id != lease_id || existing.client_id != client_id {
            return Err(DaemonError::InputLeaseMismatch(terminal_id));
        }
        leases.remove(&terminal_id);
        Ok(())
    }

    pub fn write_terminal_input(
        &self,
        client_id: ClientId,
        frame: TerminalInputFrame,
    ) -> Result<(), DaemonError> {
        let leases = lock(&self.inner.input_leases)?;
        let lease = leases
            .get(&frame.terminal_id)
            .ok_or(DaemonError::InputLeaseMissing(frame.terminal_id))?;
        if lease.lease_id != frame.lease_id || lease.client_id != client_id {
            return Err(DaemonError::InputLeaseMismatch(frame.terminal_id));
        }
        self.inner
            .terminals
            .get(frame.terminal_id)?
            .write_input(frame.sequence, &frame.bytes)?;
        Ok(())
    }

    pub fn block_snapshot(&self, task_id: TaskId) -> Result<BlockDocument, DaemonError> {
        let authority = lock(&self.inner.authority)?;
        authority
            .projectors
            .get(&task_id)
            .ok_or(DaemonError::TaskNotFound(task_id))?
            .snapshot()
            .map_err(Into::into)
    }

    pub fn block_revision(&self, task_id: TaskId) -> Result<u64, DaemonError> {
        let authority = lock(&self.inner.authority)?;
        Ok(authority
            .projectors
            .get(&task_id)
            .ok_or(DaemonError::TaskNotFound(task_id))?
            .revision())
    }

    fn reconcile_interrupted_dispatches(&self) -> Result<(), DaemonError> {
        let interrupted = {
            let authority = lock(&self.inner.authority)?;
            authority
                .operations
                .records()
                .filter(|record| record.state == OperationState::Dispatching)
                .cloned()
                .collect::<Vec<_>>()
        };
        for record in interrupted {
            self.transition(
                record.task_id,
                record.operation_id,
                record.revision,
                OperationState::UnknownExecution,
                Actor::System,
                Some("daemon restarted without a reattachable PTY receipt".into()),
            )?;
        }
        Ok(())
    }

    fn reconcile_unrecoverable_sandbox_authorizations(&self) -> Result<(), DaemonError> {
        let authorizations = {
            let authority = lock(&self.inner.authority)?;
            authority
                .operations
                .records()
                .filter(|record| {
                    record.state == OperationState::Authorized
                        && matches!(record.action, OperationAction::Shell { .. })
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        for record in authorizations {
            self.transition(
                record.task_id,
                record.operation_id,
                record.revision,
                OperationState::Failed,
                Actor::System,
                Some("daemon restart invalidated the in-memory one-use sandbox lease".into()),
            )?;
        }
        Ok(())
    }

    fn monitor_terminal(
        &self,
        session: TerminalSessionHandle,
        subscription: TerminalSubscription,
        terminal_id: TerminalId,
    ) {
        let Ok(context) = self.terminal_context(terminal_id) else {
            return;
        };
        let operation_context = match context {
            TerminalContext::Operation(context) => Some(context),
            TerminalContext::UserShell => None,
        };
        let mut observation = OutputObservation::default();
        match subscription.replay {
            TerminalReplay::Chunks(chunks) => {
                for chunk in chunks {
                    if let Some(context) = operation_context {
                        observation.observe(chunk.sequence, chunk.bytes.len() as u64);
                        self.flush_observation_if_needed(
                            context,
                            terminal_id,
                            &mut observation,
                            false,
                        );
                    }
                }
            }
            TerminalReplay::SnapshotRequired(snapshot) => {
                if let Some(context) = operation_context {
                    observation.observe_snapshot(&snapshot);
                    self.flush_observation_if_needed(context, terminal_id, &mut observation, true);
                }
            }
        }

        if let Some(exit) = subscription.exit {
            if let Some(context) = operation_context {
                self.flush_observation_if_needed(context, terminal_id, &mut observation, true);
            }
            self.finalize_terminal(context, terminal_id, exit.exit_code);
            return;
        }

        loop {
            match subscription.receiver.recv() {
                Ok(TerminalEvent::Output(chunk)) => {
                    if let Some(context) = operation_context {
                        observation.observe(chunk.sequence, chunk.bytes.len() as u64);
                        self.flush_observation_if_needed(
                            context,
                            terminal_id,
                            &mut observation,
                            false,
                        );
                    }
                }
                Ok(TerminalEvent::Exited(exit)) => {
                    if let Some(context) = operation_context {
                        self.flush_observation_if_needed(
                            context,
                            terminal_id,
                            &mut observation,
                            true,
                        );
                    }
                    self.finalize_terminal(context, terminal_id, exit.exit_code);
                    break;
                }
                Ok(TerminalEvent::Fault(message)) => {
                    if let Some(context) = operation_context {
                        let _ = self.record(NewEvent {
                            task_id: context.task_id,
                            run_id: None,
                            operation_id: Some(context.operation_id),
                            causation_id: None,
                            correlation_id: None,
                            payload: DomainEvent::Diagnostic {
                                code: "pty_read_fault".into(),
                                message,
                            },
                        });
                    }
                }
                Err(_) => {
                    let snapshot = session.snapshot();
                    if let Some(context) = operation_context {
                        observation.observe_snapshot(&snapshot);
                        self.flush_observation_if_needed(
                            context,
                            terminal_id,
                            &mut observation,
                            true,
                        );
                    }
                    if let Some(exit) = snapshot.exit {
                        self.finalize_terminal(context, terminal_id, exit.exit_code);
                    } else if let Some(context) = operation_context {
                        let _ = self.transition_current(
                            context,
                            OperationState::UnknownExecution,
                            "terminal monitor fell behind the bounded channel",
                        );
                    }
                    break;
                }
            }
        }
    }

    fn flush_observation_if_needed(
        &self,
        context: OperationTerminalContext,
        terminal_id: TerminalId,
        observation: &mut OutputObservation,
        force: bool,
    ) {
        if observation.pending_bytes == 0
            || (!force && observation.pending_bytes < OBSERVATION_BATCH_BYTES)
        {
            return;
        }
        let bytes = observation.pending_bytes;
        let sequence = observation.last_sequence;
        if self
            .record(NewEvent {
                task_id: context.task_id,
                run_id: None,
                operation_id: Some(context.operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::TerminalOutputObserved {
                    terminal_id,
                    stream_sequence: sequence,
                    byte_count: bytes,
                },
            })
            .is_ok()
        {
            observation.pending_bytes = 0;
            observation.recorded_bytes = observation.recorded_bytes.saturating_add(bytes);
        }
    }

    fn finalize_terminal(
        &self,
        context: TerminalContext,
        terminal_id: TerminalId,
        exit_code: Option<u32>,
    ) {
        let cancelled = lock(&self.inner.cancelled_terminals)
            .map(|mut terminals| terminals.remove(&terminal_id))
            .unwrap_or(false);
        let sandbox_execution = lock(&self.inner.sandbox_executions)
            .ok()
            .and_then(|mut executions| executions.remove(&terminal_id));
        if let TerminalContext::Operation(context) = context {
            let _ = self.record(NewEvent {
                task_id: context.task_id,
                run_id: None,
                operation_id: Some(context.operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::TerminalExited {
                    terminal_id,
                    exit_code,
                },
            });
            let target = if cancelled {
                OperationState::Cancelled
            } else if exit_code == Some(0) {
                OperationState::Succeeded
            } else {
                OperationState::Failed
            };
            if let Some(execution) = &sandbox_execution {
                let operation_revision = self
                    .operation(context.operation_id)
                    .map(|record| record.revision)
                    .unwrap_or(0);
                if operation_revision != 0 {
                    let outcome = if cancelled {
                        SandboxOutcome::Failed
                    } else if exit_code == Some(0) {
                        SandboxOutcome::Succeeded
                    } else {
                        SandboxOutcome::Failed
                    };
                    let _ = self.record_sandbox_receipt(
                        context.task_id,
                        context.operation_id,
                        operation_revision,
                        &execution.compiled,
                        execution.started_at_ms,
                        now_ms().unwrap_or(execution.started_at_ms),
                        outcome,
                        exit_code,
                    );
                }
            }
            let _ = self.transition_current(context, target, "PTY exited");
        }
        if let Ok(mut contexts) = lock(&self.inner.terminal_contexts) {
            contexts.remove(&terminal_id);
        }
        if let Ok(mut leases) = lock(&self.inner.input_leases) {
            leases.remove(&terminal_id);
        }
        if let Some(execution) = sandbox_execution {
            cleanup_scratch_directory(&execution.scratch_directory);
        }
    }

    fn transition_current(
        &self,
        context: OperationTerminalContext,
        target: OperationState,
        reason: &str,
    ) -> Result<(), DaemonError> {
        let record = self.operation(context.operation_id)?;
        if record.state != OperationState::Dispatching {
            return Ok(());
        }
        self.transition(
            context.task_id,
            context.operation_id,
            record.revision,
            target,
            Actor::System,
            Some(reason.into()),
        )?;
        Ok(())
    }

    fn transition(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        to: OperationState,
        actor: Actor,
        reason: Option<String>,
    ) -> Result<OperationRecord, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        self.record(NewEvent {
            task_id,
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload: DomainEvent::OperationStateChanged {
                revision: expected_revision + 1,
                from: record.state,
                to,
                actor,
                reason,
            },
        })?;
        self.operation(operation_id)
    }

    pub(crate) fn operation(
        &self,
        operation_id: OperationId,
    ) -> Result<OperationRecord, DaemonError> {
        lock(&self.inner.authority)?
            .operations
            .get(operation_id)
            .cloned()
            .ok_or(DaemonError::OperationNotFound(operation_id))
    }

    fn terminal_context(&self, terminal_id: TerminalId) -> Result<TerminalContext, DaemonError> {
        lock(&self.inner.terminal_contexts)?
            .get(&terminal_id)
            .copied()
            .ok_or(DaemonError::TerminalContextMissing(terminal_id))
    }

    fn require_task(&self, task_id: TaskId) -> Result<(), DaemonError> {
        if lock(&self.inner.authority)?
            .projectors
            .contains_key(&task_id)
        {
            Ok(())
        } else {
            Err(DaemonError::TaskNotFound(task_id))
        }
    }

    fn record(&self, event: NewEvent) -> Result<(), DaemonError> {
        let (event, patch) = {
            let mut authority = lock(&self.inner.authority)?;
            let creating_task = matches!(event.payload, DomainEvent::TaskCreated { .. });
            if creating_task && authority.projectors.contains_key(&event.task_id) {
                return Err(DaemonError::DuplicateTask(event.task_id));
            }
            if !creating_task && !authority.projectors.contains_key(&event.task_id) {
                return Err(DaemonError::TaskNotFound(event.task_id));
            }
            let envelope = authority.journal.prepare(event)?;
            let mut next_operations = authority.operations.clone();
            next_operations.apply(&envelope)?;
            let mut next_projector = authority
                .projectors
                .get(&envelope.task_id)
                .cloned()
                .unwrap_or_else(|| BlockProjector::new(envelope.task_id));
            let patch = next_projector.apply(&envelope)?;
            authority.journal.append_envelope(envelope.clone())?;
            authority.operations = next_operations;
            authority
                .projectors
                .insert(envelope.task_id, next_projector);
            (envelope, patch)
        };
        self.broadcast(ControlResponse::Event {
            event: Box::new(event.clone()),
        });
        self.broadcast(ControlResponse::BlockPatch {
            patch: patch.clone(),
        });
        self.broadcast_block_patch(event.task_id, patch);
        Ok(())
    }

    pub(crate) fn subscribe_block_patches(
        &self,
    ) -> Result<Receiver<(TaskId, BlockPatch)>, DaemonError> {
        let (sender, receiver) = bounded(BLOCK_SUBSCRIBER_CAPACITY);
        lock(&self.inner.block_subscribers)?.push(sender);
        Ok(receiver)
    }

    fn broadcast_block_patch(&self, task_id: TaskId, patch: BlockPatch) {
        let Ok(mut subscribers) = self.inner.block_subscribers.lock() else {
            return;
        };
        subscribers.retain(|sender| match sender.try_send((task_id, patch.clone())) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
    }

    fn subscribe_control(&self) -> Result<Receiver<ControlResponse>, DaemonError> {
        let (sender, receiver) = bounded(CONTROL_SUBSCRIBER_CAPACITY);
        lock(&self.inner.control_subscribers)?.push(sender);
        Ok(receiver)
    }

    fn broadcast(&self, response: ControlResponse) {
        let Ok(mut subscribers) = self.inner.control_subscribers.lock() else {
            return;
        };
        subscribers.retain(|sender| match sender.try_send(response.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
    }

    fn release_client(&self, client_id: ClientId) {
        if let Ok(mut leases) = self.inner.input_leases.lock() {
            leases.retain(|_, lease| lease.client_id != client_id);
        }
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

fn validate_action_kind(kind: &OperationKind, action: &OperationAction) -> Result<(), DaemonError> {
    let valid = match (kind, action) {
        (OperationKind::Shell, OperationAction::Shell { .. }) => true,
        (OperationKind::McpServerLaunch, OperationAction::McpServerLaunch { launch }) => {
            return validate_mcp_server_launch(launch);
        }
        (OperationKind::McpTool, OperationAction::McpToolCall { call }) => {
            return validate_local_mcp_tool_call(call);
        }
        (
            OperationKind::McpTool
            | OperationKind::FileEdit
            | OperationKind::AgentTool
            | OperationKind::ComputerUse
            | OperationKind::ArtifactBuild
            | OperationKind::Other(_),
            OperationAction::Opaque { .. },
        ) => true,
        _ => false,
    };
    if !valid {
        return Err(DaemonError::ActionKindMismatch);
    }
    Ok(())
}

fn validate_local_mcp_tool_call(call: &LocalMcpToolCall) -> Result<(), DaemonError> {
    if call.schema_version != hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION
        || call.server_id.is_empty()
        || call.server_id.len() > 64
        || !call
            .server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !valid_mcp_receipt_text(&call.tool_name, 256)
    {
        return Err(DaemonError::InvalidLocalMcpToolCall);
    }
    Ok(())
}

fn validate_local_mcp_tool_call_receipt(
    receipt: &LocalMcpToolCallReceipt,
) -> Result<(), DaemonError> {
    validate_local_mcp_tool_call(&receipt.call)
        .map_err(|_| DaemonError::InvalidLocalMcpToolCallReceipt)?;
    if receipt.schema_version != hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION {
        return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
    }
    let encoded =
        serde_json::to_vec(receipt).map_err(|_| DaemonError::InvalidLocalMcpToolCallReceipt)?;
    if encoded.len() > 16 * 1024 {
        return Err(DaemonError::InvalidLocalMcpToolCallReceipt);
    }
    Ok(())
}

fn validate_mcp_server_launch(
    launch: &hyper_term_protocol::LocalMcpServerLaunch,
) -> Result<(), DaemonError> {
    if launch.schema_version != hyper_term_protocol::LOCAL_MCP_LAUNCH_SCHEMA_VERSION
        || launch.server_id.is_empty()
        || launch.server_id.len() > 64
        || !launch
            .server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || launch.argument_count > 64
        || !launch.executable.is_absolute()
        || !launch.working_directory.is_absolute()
        || !is_sha256(&launch.executable_sha256)
        || launch
            .roots_snapshot_sha256
            .as_deref()
            .is_some_and(|digest| !is_sha256(digest))
    {
        return Err(DaemonError::InvalidMcpServerLaunch);
    }
    let executable =
        fs::canonicalize(&launch.executable).map_err(|_| DaemonError::InvalidMcpServerLaunch)?;
    let working_directory = fs::canonicalize(&launch.working_directory)
        .map_err(|_| DaemonError::InvalidMcpServerLaunch)?;
    if executable != launch.executable
        || !executable.is_file()
        || working_directory != launch.working_directory
        || !working_directory.is_dir()
    {
        return Err(DaemonError::InvalidMcpServerLaunch);
    }
    Ok(())
}

#[derive(Serialize)]
struct McpToolContractIdentity<'a> {
    planned_runtime_identity: &'a str,
    name: &'a str,
    input_schema_sha256: &'a str,
    output_schema_sha256: Option<&'a str>,
}

#[derive(Serialize)]
struct NegotiatedMcpRuntimeIdentity<'a> {
    planned_runtime_identity: &'a str,
    negotiated_protocol_version: &'a str,
    server_name: &'a str,
    server_version: &'a str,
    enforced_sandbox_profile_digest: &'a str,
    capabilities_digest: &'a str,
    catalog_digest: &'a str,
}

fn validate_local_mcp_runtime_receipt(
    receipt: &LocalMcpServerRuntimeReceipt,
) -> Result<(), DaemonError> {
    validate_mcp_server_launch(&receipt.launch)
        .map_err(|_| DaemonError::InvalidLocalMcpRuntimeReceipt)?;
    if receipt.schema_version != hyper_term_protocol::LOCAL_MCP_LAUNCH_SCHEMA_VERSION
        || !valid_mcp_receipt_text(&receipt.negotiated_protocol_version, 128)
        || !valid_mcp_receipt_text(&receipt.server_name, 256)
        || !valid_mcp_receipt_text(&receipt.server_version, 128)
        || receipt.enforced_sandbox_profile_digest != receipt.launch.sandbox_profile_digest
        || receipt.credential_scope != receipt.launch.credential_scope
        || receipt.per_call_isolation
        || receipt.tools.len() > 256
    {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }

    let mut previous_name: Option<&str> = None;
    for tool in &receipt.tools {
        if !valid_mcp_receipt_text(&tool.name, 256)
            || previous_name.is_some_and(|previous| previous >= tool.name.as_str())
            || !is_sha256(&tool.input_schema_sha256)
            || tool
                .output_schema_sha256
                .as_deref()
                .is_some_and(|digest| !is_sha256(digest))
        {
            return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
        }
        let expected_contract = mcp_receipt_sha256(&McpToolContractIdentity {
            planned_runtime_identity: receipt.launch.runtime_identity_digest.as_str(),
            name: &tool.name,
            input_schema_sha256: &tool.input_schema_sha256,
            output_schema_sha256: tool.output_schema_sha256.as_deref(),
        })?;
        if tool.contract_digest.as_str() != expected_contract {
            return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
        }
        previous_name = Some(&tool.name);
    }

    let expected_catalog = mcp_receipt_sha256(&receipt.tools)?;
    if receipt.catalog_digest.as_str() != expected_catalog {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }
    let expected_runtime = mcp_receipt_sha256(&NegotiatedMcpRuntimeIdentity {
        planned_runtime_identity: receipt.launch.runtime_identity_digest.as_str(),
        negotiated_protocol_version: &receipt.negotiated_protocol_version,
        server_name: &receipt.server_name,
        server_version: &receipt.server_version,
        enforced_sandbox_profile_digest: receipt.enforced_sandbox_profile_digest.as_str(),
        capabilities_digest: receipt.capabilities_digest.as_str(),
        catalog_digest: receipt.catalog_digest.as_str(),
    })?;
    if receipt.runtime_identity_digest.as_str() != expected_runtime {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }
    let encoded =
        serde_json::to_vec(receipt).map_err(|_| DaemonError::InvalidLocalMcpRuntimeReceipt)?;
    if encoded.len() > 512 * 1024 {
        return Err(DaemonError::InvalidLocalMcpRuntimeReceipt);
    }
    Ok(())
}

fn valid_mcp_receipt_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn mcp_receipt_sha256(value: &impl Serialize) -> Result<String, DaemonError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| DaemonError::InvalidLocalMcpRuntimeReceipt)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_agent_tool_call(call: &hyper_term_protocol::AgentToolCall) -> Result<(), DaemonError> {
    if call.tool_call_id.is_empty()
        || call.tool_call_id.len() > 4096
        || call.title.is_empty()
        || call.title.len() > 16 * 1024
        || call.content.len() > 128
        || call.locations.len() > 128
    {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent tool call identity, title, or collection count is invalid".into(),
        ));
    }
    let encoded = serde_json::to_vec(call)
        .map_err(|error| DaemonError::InvalidAgentProjection(error.to_string()))?;
    if encoded.len() > 512 * 1024 {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent tool call exceeds the 512 KiB journal bound".into(),
        ));
    }
    Ok(())
}

fn validate_agent_execution_context(
    provider_id: &str,
    protocol: &str,
    thread_id: &str,
    receipts: &[ContextReceipt],
) -> Result<(), DaemonError> {
    if provider_id.is_empty()
        || provider_id.len() > 64
        || !provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || protocol.is_empty()
        || protocol.len() > 64
        || !protocol
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || thread_id.is_empty()
        || thread_id.len() > 4096
        || thread_id.chars().any(char::is_control)
        || receipts.is_empty()
        || receipts.len() > 4
    {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent execution-context identity or receipt count is invalid".into(),
        ));
    }
    let mut context_ids = HashSet::new();
    for receipt in receipts {
        if receipt.schema_version != EXECUTION_CONTEXT_SCHEMA_VERSION
            || receipt.context_id.is_empty()
            || receipt.context_id.len() > 128
            || !context_ids.insert(receipt.context_id.as_str())
            || receipt.bindings.len() > 128
            || receipt.credential_bindings.len() > 32
            || receipt.credential_bindings.iter().any(|credential| {
                credential.binding_id.is_empty()
                    || credential.binding_id.len() > 128
                    || credential.reference.provider_id.is_empty()
                    || credential.reference.provider_id.len() > 128
                    || credential.reference.secret_id.is_empty()
                    || credential.reference.secret_id.len() > 256
                    || credential.target_name.is_empty()
                    || credential.target_name.len() > 128
                    || credential.audience.is_empty()
                    || credential.audience.len() > 2048
                    || credential.audience.chars().any(char::is_control)
            })
        {
            return Err(DaemonError::InvalidAgentProjection(
                "Agent execution-context receipt is invalid or unbounded".into(),
            ));
        }
    }
    let encoded = serde_json::to_vec(receipts)
        .map_err(|error| DaemonError::InvalidAgentProjection(error.to_string()))?;
    if encoded.len() > 256 * 1024 {
        return Err(DaemonError::InvalidAgentProjection(
            "Agent execution-context receipts exceed the 256 KiB journal bound".into(),
        ));
    }
    Ok(())
}

fn validate_operation_scope(
    record: &OperationRecord,
    task_id: TaskId,
    expected_revision: u64,
) -> Result<(), DaemonError> {
    if record.task_id != task_id {
        return Err(DaemonError::OperationTaskMismatch {
            expected: record.task_id,
            actual: task_id,
        });
    }
    if record.revision != expected_revision {
        return Err(DaemonError::StaleOperationRevision {
            expected: record.revision,
            actual: expected_revision,
        });
    }
    Ok(())
}

fn recover_completed_isolated_results(
    manager: &IsolatedWorktreeManager,
    root: &Path,
    operations: &OperationReducer,
) -> Result<HashMap<OperationId, IsolatedResult>, DaemonError> {
    let mut recovered = HashMap::new();
    for operation_entry in fs::read_dir(root)?.take(1_025) {
        let operation_entry = operation_entry?;
        let metadata = operation_entry.file_type()?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(DaemonError::InvalidIsolatedResultStore);
        }
        if recovered.len() == 1_024 {
            return Err(DaemonError::InvalidIsolatedResultStore);
        }
        let operation_text = operation_entry
            .file_name()
            .into_string()
            .map_err(|_| DaemonError::InvalidIsolatedResultStore)?;
        let operation_id = OperationId::from(
            Uuid::parse_str(&operation_text)
                .map_err(|_| DaemonError::InvalidIsolatedResultStore)?,
        );
        let operation = operations
            .records()
            .find(|record| record.operation_id == operation_id)
            .ok_or(DaemonError::InvalidIsolatedResultStore)?;
        if !operation
            .required_capabilities
            .iter()
            .any(|capability| capability == ISOLATED_TASK_CAPABILITY)
        {
            return Err(DaemonError::InvalidIsolatedResultStore);
        }
        let mut completed = None;
        let mut removed_interrupted = false;
        for environment_entry in fs::read_dir(operation_entry.path())?.take(3) {
            let environment_entry = environment_entry?;
            if !environment_entry.file_type()?.is_dir() {
                return Err(DaemonError::InvalidIsolatedResultStore);
            }
            let environment = manager.reopen(environment_entry.path())?;
            if environment.manifest.task_id != operation_id.to_string() {
                return Err(DaemonError::InvalidIsolatedResultStore);
            }
            if !environment
                .environment_root
                .join("task-receipt.json")
                .is_file()
            {
                cleanup_interrupted_lima_environment(&environment.manifest.environment_id)?;
                manager.destroy(&environment)?;
                removed_interrupted = true;
                continue;
            }
            if completed.is_some() {
                return Err(DaemonError::InvalidIsolatedResultStore);
            }
            let receipt = read_isolated_task_receipt(&environment)?;
            if manager.inspect_changes(&environment)? != receipt.changes {
                return Err(DaemonError::IsolatedResultDigestMismatch);
            }
            completed = Some(IsolatedResult {
                environment,
                scratch_directory: operation_entry.path(),
                receipt,
            });
        }
        if let Some(result) = completed {
            recovered.insert(operation_id, result);
        } else if removed_interrupted {
            cleanup_scratch_directory(&operation_entry.path());
        }
    }
    Ok(recovered)
}

fn recover_isolated_acceptances(
    root: &Path,
    operations: &OperationReducer,
    results: &HashMap<OperationId, IsolatedResult>,
) -> Result<HashMap<OperationId, IsolatedAcceptance>, DaemonError> {
    let mut recovered = HashMap::new();
    let mut recovered_sources = HashSet::new();
    for entry in fs::read_dir(root)?.take(1_025) {
        let entry = entry?;
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
        if file_name.starts_with('.') && file_name.ends_with(".tmp") {
            if !entry.file_type()?.is_file() {
                return Err(DaemonError::InvalidIsolatedAcceptanceStore);
            }
            fs::remove_file(entry.path())?;
            continue;
        }
        if recovered.len() == 1_024 || !entry.file_type()?.is_file() {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        let operation_text = file_name
            .strip_suffix(".json")
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        let operation_id = OperationId::from(
            Uuid::parse_str(operation_text)
                .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?,
        );
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_ISOLATED_ACCEPTANCE_BYTES
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        let bytes = fs::read(entry.path())?;
        let stored: StoredIsolatedAcceptance = serde_json::from_slice(&bytes)
            .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
        if stored.schema_version != ISOLATED_ACCEPTANCE_SCHEMA_VERSION
            || stored.acceptance_operation_id != operation_id
            || !is_sha256(&stored.binding_digest)
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        validate_workspace_apply_set(&stored.plan)
            .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
        let operation = operations
            .records()
            .find(|record| record.operation_id == operation_id)
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        if operation.task_id != stored.task_id
            || !matches!(
                &operation.action,
                OperationAction::Opaque { kind, payload_digest }
                    if kind == "hyper_term.tier2.accept"
                        && payload_digest == &stored.binding_digest
            )
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        let source_operation = operations
            .records()
            .find(|record| record.operation_id == stored.source_operation_id)
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        let result = results
            .get(&stored.source_operation_id)
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        if source_operation.task_id != stored.task_id
            || result.environment.manifest.source_workspace != stored.workspace
            || isolated_acceptance_digest(
                stored.source_operation_id,
                &result.receipt.changes.inventory_sha256,
                &stored.workspace,
                &stored.plan,
            )? != stored.binding_digest
            || stored.plan.plans.iter().any(|plan| {
                !result.receipt.changes.changed_files.iter().any(|change| {
                    change.path == Path::new(&plan.target_path)
                        && if plan.deletes_target() {
                            change.kind == hyper_term_sandbox::IsolatedChangeKind::Deleted
                                && change.content_sha256.is_none()
                        } else {
                            change.content_sha256.as_deref() == Some(&plan.proposed_digest)
                        }
                })
            })
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        if !matches!(
            operation.state,
            OperationState::WaitingHuman
                | OperationState::Authorized
                | OperationState::Dispatching
                | OperationState::UnknownExecution
        ) {
            fs::remove_file(entry.path())?;
            continue;
        }
        if !recovered_sources.insert(stored.source_operation_id) {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        recovered.insert(
            operation_id,
            IsolatedAcceptance {
                source_operation_id: stored.source_operation_id,
                workspace: stored.workspace,
                plan: stored.plan,
                binding_digest: stored.binding_digest,
            },
        );
    }
    acceptance_root_file(root)?.sync_all()?;
    Ok(recovered)
}

fn isolated_acceptance_path(root: &Path, operation_id: OperationId) -> PathBuf {
    root.join(format!("{operation_id}.json"))
}

fn acceptance_root_file(root: &Path) -> Result<File, DaemonError> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?)
}

fn write_isolated_acceptance(
    root: &Path,
    stored: &StoredIsolatedAcceptance,
) -> Result<(), DaemonError> {
    let bytes =
        serde_json::to_vec(stored).map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
    if bytes.len() as u64 > MAX_ISOLATED_ACCEPTANCE_BYTES {
        return Err(DaemonError::InvalidIsolatedAcceptanceStore);
    }
    let target = isolated_acceptance_path(root, stored.acceptance_operation_id);
    if target.exists() {
        return Err(DaemonError::InvalidIsolatedAcceptanceStore);
    }
    let temporary = root.join(format!(
        ".{}.{}.tmp",
        stored.acceptance_operation_id,
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        acceptance_root_file(root)?.sync_all()?;
        Ok::<(), DaemonError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn remove_isolated_acceptance(root: &Path, operation_id: OperationId) -> Result<(), DaemonError> {
    match fs::remove_file(isolated_acceptance_path(root, operation_id)) {
        Ok(()) => acceptance_root_file(root)?.sync_all().map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn safe_isolated_result_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(name) if name != ".git"))
}

fn isolated_acceptance_changes(plan: &WorkspaceApplySetPlan) -> Vec<IsolatedAcceptanceChange> {
    plan.plans
        .iter()
        .map(|plan| IsolatedAcceptanceChange {
            target_path: plan.target_path.clone(),
            base_digest: plan.base_digest().map(str::to_owned),
            proposed_digest: plan.proposed_digest.clone(),
            deleted: plan.deletes_target(),
            binary: plan.is_binary(),
            base_bytes: plan.base_bytes_len(),
            proposed_bytes: plan.proposed_bytes_len(),
            before: plan.base_content().to_owned(),
            after: plan.proposed_content.clone(),
        })
        .collect()
}

fn isolated_acceptance_digest(
    source_operation_id: OperationId,
    inventory_sha256: &str,
    workspace: &Path,
    plan: &WorkspaceApplySetPlan,
) -> Result<String, DaemonError> {
    let plan = serde_json::to_vec(plan).map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
    let mut digest = Sha256::new();
    digest.update(b"hyper-term-tier2-acceptance-v2\0");
    digest.update(source_operation_id.to_string().as_bytes());
    digest.update([0]);
    digest.update(inventory_sha256.as_bytes());
    digest.update([0]);
    digest.update(workspace.as_os_str().as_bytes());
    digest.update([0]);
    digest.update(plan);
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn isolated_acceptance_summary(
    source_operation_id: OperationId,
    target_paths: &[String],
) -> String {
    const MAX_PATH_PREVIEW_BYTES: usize = 320;
    let mut preview = String::new();
    for path in target_paths {
        let separator_bytes = usize::from(!preview.is_empty()) * 2;
        if preview.len() + separator_bytes + path.len() > MAX_PATH_PREVIEW_BYTES {
            if !preview.is_empty() {
                preview.push_str(", ");
            }
            preview.push_str("more files");
            break;
        }
        if !preview.is_empty() {
            preview.push_str(", ");
        }
        preview.push_str(path);
    }
    format!(
        "Apply {} reviewed Tier 2 file(s) from operation {source_operation_id}: {preview}",
        target_paths.len()
    )
}

fn bounded_nonempty(
    value: String,
    maximum: usize,
    label: &'static str,
) -> Result<String, DaemonError> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > maximum {
        Err(DaemonError::InvalidBoundedText { label, maximum })
    } else {
        Ok(value)
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn now_ms() -> Result<u64, DaemonError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DaemonError::ClockBeforeUnixEpoch)?
        .as_millis();
    u64::try_from(milliseconds).map_err(|_| DaemonError::ClockOutOfRange)
}

fn create_private_directory(path: &Path) -> Result<(), DaemonError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn cleanup_scratch_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, DaemonError> {
    mutex.lock().map_err(|_| DaemonError::LockPoisoned)
}

#[cfg(unix)]
mod unix_server {
    use std::io;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    pub struct UnixServerHandle {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Drop for UnixServerHandle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = fs::remove_file(&self.path);
        }
    }

    pub fn spawn_unix_server(
        path: impl AsRef<Path>,
        state: DaemonState,
    ) -> Result<UnixServerHandle, DaemonError> {
        let path = path.as_ref().to_path_buf();
        let listener = bind_socket(&path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("hyperd-accept".into())
            .spawn(move || accept_until_stopped(listener, state, thread_stop))?;
        Ok(UnixServerHandle {
            path,
            stop,
            thread: Some(thread),
        })
    }

    pub fn run_unix_server(path: impl AsRef<Path>, state: DaemonState) -> Result<(), DaemonError> {
        let path = path.as_ref().to_path_buf();
        let listener = bind_socket(&path)?;
        let _cleanup = SocketCleanup(path);
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => spawn_connection(stream, state.clone())?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    struct SocketCleanup(PathBuf);

    impl Drop for SocketCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn bind_socket(path: &Path) -> Result<UnixListener, DaemonError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if !metadata.file_type().is_socket() {
                return Err(DaemonError::UnsafeSocketPath(path.to_path_buf()));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(DaemonError::SocketInUse(path.to_path_buf()));
            }
            fs::remove_file(path)?;
        }
        Ok(UnixListener::bind(path)?)
    }

    fn accept_until_stopped(listener: UnixListener, state: DaemonState, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = spawn_connection(stream, state.clone());
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    }

    fn spawn_connection(stream: UnixStream, state: DaemonState) -> Result<(), DaemonError> {
        // A nonblocking listener can yield a nonblocking accepted socket on
        // some Unix hosts. Each connection has its own reader thread, so keep
        // the framed protocol blocking and avoid treating a handshake race as
        // an invalid frame.
        stream.set_nonblocking(false)?;
        thread::Builder::new()
            .name("hyperd-client".into())
            .spawn(move || {
                let _ = handle_connection(stream, state);
            })?;
        Ok(())
    }

    #[derive(Clone)]
    struct ConnectionWriter {
        stream: Arc<Mutex<UnixStream>>,
    }

    impl ConnectionWriter {
        fn new(stream: UnixStream) -> Self {
            Self {
                stream: Arc::new(Mutex::new(stream)),
            }
        }

        fn send(&self, frame: &WireFrame) -> Result<(), DaemonError> {
            write_frame(&mut *lock(&self.stream)?, frame)?;
            Ok(())
        }

        fn response(
            &self,
            request_id: Option<RequestId>,
            response: ControlResponse,
        ) -> Result<(), DaemonError> {
            self.send(&WireFrame::Response(ControlResponseEnvelope {
                request_id,
                response,
            }))
        }
    }

    fn handle_connection(stream: UnixStream, state: DaemonState) -> Result<(), DaemonError> {
        let mut reader = stream.try_clone()?;
        let writer = ConnectionWriter::new(stream);
        let (client_id, hello_request) = match read_frame(&mut reader)? {
            WireFrame::Request(ControlRequestEnvelope {
                request_id,
                request:
                    ControlRequest::Hello {
                        client_id,
                        protocol_version,
                    },
            }) => {
                if protocol_version != PROTOCOL_VERSION {
                    writer.response(
                        Some(request_id),
                        ControlResponse::Error {
                            code: "unsupported_protocol".into(),
                            message: format!(
                                "client requested {protocol_version}, daemon supports {PROTOCOL_VERSION}"
                            ),
                        },
                    )?;
                    return Ok(());
                }
                (client_id, request_id)
            }
            _ => return Err(DaemonError::HelloRequired),
        };
        // Register the event stream before acknowledging the handshake. A
        // client may act as soon as `connect` returns, so sending `Welcome`
        // first leaves a window where authority events can be lost.
        let control = state.subscribe_control()?;
        writer.response(
            Some(hello_request),
            ControlResponse::Welcome {
                protocol_version: PROTOCOL_VERSION,
                daemon_instance: state.instance_id(),
            },
        )?;
        let _lease_cleanup = ClientLeaseCleanup {
            state: state.clone(),
            client_id,
        };

        let control_writer = writer.clone();
        thread::Builder::new()
            .name(format!("hyperd-events-{client_id}"))
            .spawn(move || {
                while let Ok(response) = control.recv() {
                    if control_writer.response(None, response).is_err() {
                        break;
                    }
                }
            })?;

        loop {
            let frame = match read_frame(&mut reader) {
                Ok(frame) => frame,
                Err(WireError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::BrokenPipe
                    ) =>
                {
                    break;
                }
                Err(error) => {
                    let _ = writer.response(
                        None,
                        ControlResponse::Error {
                            code: "invalid_frame".into(),
                            message: error.to_string(),
                        },
                    );
                    break;
                }
            };
            match frame {
                WireFrame::Request(request) => {
                    handle_request(&state, &writer, client_id, request)?;
                }
                WireFrame::TerminalInput(frame) => {
                    if let Err(error) = state.write_terminal_input(client_id, frame) {
                        writer.response(None, error_response(&error))?;
                    }
                }
                WireFrame::Response(_)
                | WireFrame::TerminalOutput(_)
                | WireFrame::TerminalSnapshot(_) => {
                    writer.response(
                        None,
                        ControlResponse::Error {
                            code: "invalid_client_frame".into(),
                            message: "client sent a daemon-only frame".into(),
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    struct ClientLeaseCleanup {
        state: DaemonState,
        client_id: ClientId,
    }

    impl Drop for ClientLeaseCleanup {
        fn drop(&mut self) {
            self.state.release_client(self.client_id);
        }
    }

    fn handle_request(
        state: &DaemonState,
        writer: &ConnectionWriter,
        session_client_id: ClientId,
        envelope: ControlRequestEnvelope,
    ) -> Result<(), DaemonError> {
        let request_id = envelope.request_id;
        if let ControlRequest::SubscribeTerminal {
            terminal_id,
            after_sequence,
        } = envelope.request
        {
            match state.terminal_subscription(terminal_id, after_sequence) {
                Ok(subscription) => {
                    writer.response(
                        Some(request_id),
                        ControlResponse::TerminalSubscribed {
                            terminal_id,
                            after_sequence,
                        },
                    )?;
                    spawn_terminal_forwarder(writer.clone(), terminal_id, subscription)?;
                }
                Err(error) => writer.response(Some(request_id), error_response(&error))?,
            }
            return Ok(());
        }

        let response = match envelope.request {
            ControlRequest::Hello { .. } => Err(DaemonError::DuplicateHello),
            ControlRequest::CreateTask { title } => state
                .create_task(title)
                .map(|task_id| ControlResponse::TaskCreated { task_id }),
            ControlRequest::ProposeOperation {
                task_id,
                kind,
                action,
                summary,
                risk,
                required_capabilities,
            } => state
                .propose_operation(task_id, kind, action, summary, risk, required_capabilities)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::DecidePermission {
                task_id,
                operation_id,
                expected_revision,
                decision,
            } => state
                .decide_permission(task_id, operation_id, expected_revision, decision)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::BeginOperation {
                task_id,
                operation_id,
                expected_revision,
            } => state
                .begin_operation(task_id, operation_id, expected_revision)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::ExecuteBrokeredMcpTool {
                task_id,
                operation_id,
                expected_revision,
                tool_name,
                proposal_digest,
                arguments,
            } => state
                .execute_brokered_mcp_tool(
                    task_id,
                    operation_id,
                    expected_revision,
                    tool_name,
                    proposal_digest,
                    arguments,
                )
                .map(|execution| ControlResponse::BrokeredMcpToolExecuted { execution }),
            ControlRequest::CompleteOperation {
                task_id,
                operation_id,
                expected_revision,
                completion,
            } => state
                .complete_operation(task_id, operation_id, expected_revision, completion)
                .map(|record| ControlResponse::OperationUpdated {
                    operation_id: record.operation_id,
                    revision: record.revision,
                    state: record.state,
                }),
            ControlRequest::AcceptGenUiArtifact {
                task_id,
                operation_id,
                expected_revision,
                candidate,
            } => state
                .accept_genui_artifact(task_id, operation_id, expected_revision, candidate)
                .map(|artifact| ControlResponse::GenUiArtifactAccepted { artifact }),
            ControlRequest::DispatchTerminal {
                task_id,
                operation_id,
                expected_revision,
                size,
            } => state
                .dispatch_terminal(task_id, operation_id, expected_revision, size)
                .map(|terminal_id| ControlResponse::TerminalCreated { terminal_id }),
            ControlRequest::OpenUserShell { cwd, size } => state
                .open_user_shell(cwd, size)
                .map(|terminal_id| ControlResponse::TerminalCreated { terminal_id }),
            ControlRequest::ResizeTerminal {
                terminal_id,
                generation,
                size,
            } => state
                .resize_terminal(terminal_id, generation, size)
                .map(|()| ControlResponse::Ack),
            ControlRequest::CloseTerminal { terminal_id } => state
                .close_terminal(terminal_id)
                .map(|()| ControlResponse::Ack),
            ControlRequest::AcquireInputLease {
                terminal_id,
                client_id,
            } => {
                if client_id != session_client_id {
                    Err(DaemonError::ClientIdentityMismatch)
                } else {
                    state.acquire_input_lease(terminal_id, client_id).map(
                        |(lease_id, generation)| ControlResponse::InputLeaseGranted {
                            terminal_id,
                            lease_id,
                            generation,
                        },
                    )
                }
            }
            ControlRequest::ReleaseInputLease {
                terminal_id,
                lease_id,
            } => state
                .release_input_lease(terminal_id, lease_id, session_client_id)
                .map(|()| ControlResponse::Ack),
            ControlRequest::GetBlockSnapshot { task_id } => state
                .block_snapshot(task_id)
                .map(|document| ControlResponse::BlockSnapshot { document }),
            ControlRequest::SubscribeTerminal { .. } => unreachable!("handled above"),
        };
        writer.response(
            Some(request_id),
            match response {
                Ok(response) => response,
                Err(error) => error_response(&error),
            },
        )?;
        Ok(())
    }

    fn spawn_terminal_forwarder(
        writer: ConnectionWriter,
        terminal_id: TerminalId,
        subscription: TerminalSubscription,
    ) -> Result<(), DaemonError> {
        thread::Builder::new()
            .name(format!("hyperd-stream-{terminal_id}"))
            .spawn(move || {
                let replay_result = match subscription.replay {
                    TerminalReplay::Chunks(chunks) => chunks.into_iter().try_for_each(|chunk| {
                        writer.send(&WireFrame::TerminalOutput(TerminalDataFrame {
                            terminal_id,
                            sequence: chunk.sequence,
                            bytes: chunk.bytes.to_vec(),
                        }))
                    }),
                    TerminalReplay::SnapshotRequired(snapshot) => {
                        writer.send(&WireFrame::TerminalSnapshot(TerminalSnapshotFrame {
                            terminal_id,
                            base_sequence: snapshot.base_sequence,
                            next_sequence: snapshot.next_sequence,
                            total_bytes: snapshot.total_bytes,
                            bytes: snapshot.tail,
                        }))
                    }
                };
                if replay_result.is_err() {
                    return;
                }
                if let Some(exit) = subscription.exit {
                    let _ = writer.response(
                        None,
                        ControlResponse::TerminalExited {
                            terminal_id,
                            exit_code: exit.exit_code,
                        },
                    );
                    return;
                }
                while let Ok(event) = subscription.receiver.recv() {
                    let result = match event {
                        TerminalEvent::Output(chunk) => {
                            writer.send(&WireFrame::TerminalOutput(TerminalDataFrame {
                                terminal_id,
                                sequence: chunk.sequence,
                                bytes: chunk.bytes.to_vec(),
                            }))
                        }
                        TerminalEvent::Exited(exit) => {
                            let result = writer.response(
                                None,
                                ControlResponse::TerminalExited {
                                    terminal_id,
                                    exit_code: exit.exit_code,
                                },
                            );
                            let _ = result;
                            break;
                        }
                        TerminalEvent::Fault(message) => writer.response(
                            None,
                            ControlResponse::Error {
                                code: "terminal_fault".into(),
                                message,
                            },
                        ),
                    };
                    if result.is_err() {
                        break;
                    }
                }
            })?;
        Ok(())
    }

    fn error_response(error: &DaemonError) -> ControlResponse {
        ControlResponse::Error {
            code: error.code().into(),
            message: error.to_string(),
        }
    }
}

#[cfg(unix)]
pub use unix_server::{UnixServerHandle, run_unix_server, spawn_unix_server};

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
    SandboxLease(#[from] SandboxLeaseError),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error("artifact store rejected the candidate: {0}")]
    ArtifactStore(String),
    #[error("daemon state lock is poisoned")]
    LockPoisoned,
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
            | Self::InvalidLocalMcpRuntimeReceipt
            | Self::InvalidLocalMcpToolCallReceipt
            | Self::LocalMcpRuntimeNotRecorded
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
