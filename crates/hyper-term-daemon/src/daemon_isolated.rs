use super::*;

impl DaemonState {
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
}
