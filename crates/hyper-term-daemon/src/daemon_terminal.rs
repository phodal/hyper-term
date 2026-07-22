use super::*;

impl DaemonState {
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

    pub(super) fn consume_authorized_sandbox(
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
    pub(super) fn record_sandbox_receipt(
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

    pub(crate) fn pending_operation_id(
        &self,
        task_id: TaskId,
    ) -> Result<Option<OperationId>, DaemonError> {
        let authority = lock(&self.inner.authority)?;
        Ok(authority
            .projectors
            .get(&task_id)
            .ok_or(DaemonError::TaskNotFound(task_id))?
            .latest_waiting_operation_id())
    }

    pub(super) fn reconcile_interrupted_dispatches(&self) -> Result<(), DaemonError> {
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

    pub(super) fn reconcile_unrecoverable_sandbox_authorizations(&self) -> Result<(), DaemonError> {
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

    pub(super) fn transition(
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

    pub(super) fn require_task(&self, task_id: TaskId) -> Result<(), DaemonError> {
        if lock(&self.inner.authority)?
            .projectors
            .contains_key(&task_id)
        {
            Ok(())
        } else {
            Err(DaemonError::TaskNotFound(task_id))
        }
    }

    pub(super) fn record(&self, event: NewEvent) -> Result<(), DaemonError> {
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

    pub(super) fn subscribe_control(&self) -> Result<Receiver<ControlResponse>, DaemonError> {
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

    pub(super) fn release_client(&self, client_id: ClientId) {
        if let Ok(mut leases) = self.inner.input_leases.lock() {
            leases.retain(|_, lease| lease.client_id != client_id);
        }
    }
}
