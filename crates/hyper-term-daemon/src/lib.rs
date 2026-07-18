//! Out-of-process authority host for Hyper Term.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
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
    AcceptedGenUiArtifact, Actor, BlockDocument, BlockId, CapabilityLease, ClientId,
    CompiledSandboxProfile, ControlRequest, ControlRequestEnvelope, ControlResponse,
    ControlResponseEnvelope, DomainEvent, GenUiArtifactCandidate, InputLeaseId, MessageRole,
    NewEvent, OperationAction, OperationCompletion, OperationId, OperationKind, OperationOutcome,
    OperationState, PROTOCOL_VERSION, PermissionDecision, RequestId, RiskClass, SandboxEnforcement,
    SandboxEnvironmentPolicy, SandboxFileSystemPolicy, SandboxLeaseId, SandboxLifetime,
    SandboxNetworkPolicy, SandboxOutcome, SandboxPathAccess, SandboxPathRule, SandboxProcessPolicy,
    SandboxReceipt, SandboxResourceLimits, TaskId, TerminalDataFrame, TerminalId,
    TerminalInputFrame, TerminalSize, TerminalSnapshotFrame, WireError, WireFrame, read_frame,
    write_frame,
};
use hyper_term_sandbox::MacOsSeatbeltLauncher;
use thiserror::Error;
use uuid::Uuid;

mod agent_gateway;
mod artifact_store;
#[cfg(unix)]
mod client;
#[cfg(unix)]
mod mcp_gateway;
mod web_gateway;

use artifact_store::{ArtifactStore, ArtifactStoreError, StoredGenUiArtifact};

pub use agent_gateway::{
    AgentGatewayConfig, AgentGatewayError, AgentGatewayHandle, AgentGenUiRuntimeConfig,
    spawn_agent_gateway,
};
#[cfg(unix)]
pub use client::{ControlClient, ControlClientError};
#[cfg(unix)]
pub use mcp_gateway::{
    DenoGenUiMcpExecutorConfig, DenoMcpExecutorConfig, McpGatewayError, McpStdioConfig,
    run_mcp_stdio,
};
pub use web_gateway::{
    TerminalGatewayConfig, TerminalGatewayError, TerminalGatewayHandle, spawn_terminal_gateway,
};

const CONTROL_SUBSCRIBER_CAPACITY: usize = 512;
const OBSERVATION_BATCH_BYTES: u64 = 64 * 1024;
const SANDBOX_LEASE_TTL_MS: u64 = 5 * 60 * 1_000;

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
    sandbox_launcher: Arc<dyn SandboxLauncher>,
    sandbox_leases: Mutex<CapabilityLeaseLedger>,
    authorized_sandboxes: Mutex<HashMap<OperationId, AuthorizedSandbox>>,
    sandbox_executions: Mutex<HashMap<TerminalId, SandboxExecutionContext>>,
    state_directory: PathBuf,
    artifacts: ArtifactStore,
    scratch_root: PathBuf,
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
                sandbox_launcher,
                sandbox_leases: Mutex::new(CapabilityLeaseLedger::default()),
                authorized_sandboxes: Mutex::new(HashMap::new()),
                sandbox_executions: Mutex::new(HashMap::new()),
                state_directory,
                artifacts,
                scratch_root,
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
        self.transition(
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
                operation_revision: 3,
                prompt: "Allow this exact operation once?".into(),
                options: vec![
                    PermissionDecision::AllowOnce,
                    PermissionDecision::RejectOnce,
                    PermissionDecision::Cancelled,
                ],
            },
        })?;
        self.operation(operation_id)
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

        let scratch_directory = self
            .inner
            .scratch_root
            .join(record.operation_id.to_string());
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
                enforcement: SandboxEnforcement::Native,
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
                resources: SandboxResourceLimits::default(),
                lifetime: SandboxLifetime::OneOperation,
            };
            let actor = Actor::System;
            let plan = self
                .inner
                .sandbox_launcher
                .compile(&SandboxCompileRequest {
                    operation_id: record.operation_id,
                    operation_revision: authorized_revision,
                    actor: actor.clone(),
                    command: command.clone(),
                    profile,
                })?;
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
        if !matches!(record.action, OperationAction::Opaque { .. }) {
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
        if !matches!(record.action, OperationAction::Opaque { .. }) {
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
        Ok(())
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
    if matches!(kind, OperationKind::Shell) != matches!(action, OperationAction::Shell { .. }) {
        return Err(DaemonError::ActionKindMismatch);
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

        let control = state.subscribe_control()?;
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
    #[error("generic dispatch accepts only opaque operation actions")]
    GenericDispatchRequiresOpaqueAction,
    #[error("artifact acceptance requires the dispatching hyper_term.genui.compile operation")]
    ArtifactAcceptanceRequiresGenUiCompile,
    #[error("artifact {0} is not the task's current accepted artifact")]
    ArtifactNotActive(hyper_term_protocol::ArtifactId),
    #[error("{label} must contain between 1 and {maximum} bytes")]
    InvalidBoundedText { label: &'static str, maximum: usize },
    #[error("operation result digest must be a lowercase SHA-256 value")]
    InvalidResultDigest,
    #[error("operation succeeded flag conflicts with its structured outcome")]
    InconsistentOperationOutcome,
    #[error("operation kind and action payload do not match")]
    ActionKindMismatch,
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
            | Self::UnsupportedTerminalAction
            | Self::GenericDispatchRequiresOpaqueAction
            | Self::ArtifactAcceptanceRequiresGenUiCompile => "unsupported_action",
            Self::ArtifactNotActive(_) => "artifact_not_active",
            Self::ArtifactStore(_) => "artifact_error",
            Self::SandboxWorkingDirectoryRequired
            | Self::InvalidSandboxWorkingDirectory { .. }
            | Self::WorkspaceInsideDaemonState => "invalid_sandbox_workspace",
            Self::SandboxAuthorizationMissing(_)
            | Self::SandboxAuthorizationAlreadyExists(_)
            | Self::UnenforcedSandboxBackend
            | Self::Sandbox(_)
            | Self::SandboxLease(_) => "sandbox_error",
            Self::InvalidBoundedText { .. }
            | Self::InvalidResultDigest
            | Self::InconsistentOperationOutcome
            | Self::EmptyMessage
            | Self::MessageTooLarge(_)
            | Self::ExternalMessageIdTooLarge => "invalid_request",
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
