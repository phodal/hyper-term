use super::*;

impl AgentGatewayRuntime {
    pub(super) fn preview_workspace_apply(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: AgentWorkspaceApplyRequest,
    ) -> Result<AgentWorkspacePreviewResponse, WorkspaceProposalError> {
        if request.review_digest.is_some() {
            return Err(WorkspaceProposalError::InvalidRequest);
        }
        let artifact_source_revision = request.artifact_source_revision;
        let mappings = normalize_workspace_apply_mappings(request)?;
        if mappings.iter().any(|mapping| !mapping.hunk_ids.is_empty()) {
            return Err(WorkspaceProposalError::InvalidRequest);
        }
        let review = self.prepare_workspace_review(
            session_id,
            artifact_id,
            artifact_source_revision,
            &mappings,
        )?;
        Ok(workspace_preview_response(&review))
    }

    pub(super) fn prepare_workspace_review(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        artifact_source_revision: u64,
        mappings: &[AgentWorkspaceApplyMapping],
    ) -> Result<PreparedWorkspaceReview, WorkspaceProposalError> {
        let session = self
            .session(session_id)
            .map_err(|_| WorkspaceProposalError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| WorkspaceProposalError::ArtifactUnavailable)?;
        if artifact_source_revision != artifact.metadata.source_revision {
            return Err(WorkspaceProposalError::StaleRevision);
        }
        let mut target_sources = BTreeMap::new();
        let mut plan_requests = Vec::with_capacity(mappings.len());
        for mapping in mappings {
            if target_sources
                .insert(mapping.target_path.clone(), mapping.source_path.clone())
                .is_some()
            {
                return Err(WorkspaceProposalError::InvalidRequest);
            }
            let proposed_content = artifact
                .source_files
                .get(&mapping.source_path)
                .cloned()
                .ok_or(WorkspaceProposalError::InvalidRequest)?;
            plan_requests.push((mapping.target_path.clone(), proposed_content));
        }
        let plan = prepare_workspace_apply_set(&self.config.workspace, plan_requests)
            .map_err(map_workspace_prepare_error)?;
        let source_paths = plan
            .plans
            .iter()
            .map(|plan| {
                target_sources
                    .get(&plan.target_path)
                    .cloned()
                    .ok_or(WorkspaceProposalError::InvalidRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let artifact_source_digests = source_paths
            .iter()
            .map(|source_path| {
                artifact
                    .source_files
                    .get(source_path)
                    .map(|source| sha256_bytes(source.as_bytes()))
                    .ok_or(WorkspaceProposalError::InvalidRequest)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let diffs = plan
            .plans
            .iter()
            .map(|plan| {
                review_workspace_diff(
                    &plan.target_path,
                    plan.base_content(),
                    &plan.proposed_content,
                )
            })
            .collect::<Vec<_>>();
        let review_payload = serde_json::to_vec(&(
            artifact_id,
            artifact_source_revision,
            &source_paths,
            &artifact_source_digests,
            &plan,
            &diffs,
        ))
        .map_err(|_| WorkspaceProposalError::InvalidRequest)?;
        Ok(PreparedWorkspaceReview {
            artifact_source_revision,
            source_paths,
            artifact_source_digests,
            plan,
            diffs,
            review_digest: sha256_bytes(&review_payload),
        })
    }

    pub(super) fn propose_workspace_apply(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: AgentWorkspaceApplyRequest,
    ) -> Result<AgentWorkspaceApplyResponse, WorkspaceProposalError> {
        if self
            .workspace_recovery_block
            .lock()
            .map_err(|_| WorkspaceProposalError::Lock)?
            .is_some()
        {
            return Err(WorkspaceProposalError::RecoveryRequired);
        }
        let session = self
            .session(session_id)
            .map_err(|_| WorkspaceProposalError::SessionUnavailable)?;
        let artifact_source_revision = request.artifact_source_revision;
        let expected_review_digest = request.review_digest.clone();
        let mappings = normalize_workspace_apply_mappings(request)?;
        let reviewed = self.prepare_workspace_review(
            session_id,
            artifact_id,
            artifact_source_revision,
            &mappings,
        )?;
        let (source_paths, artifact_source_digests, plan, selected_hunk_count) =
            if let Some(expected_review_digest) = expected_review_digest {
                if expected_review_digest != reviewed.review_digest {
                    return Err(WorkspaceProposalError::StaleRevision);
                }
                let mut selections = BTreeMap::new();
                let mut source_paths = Vec::new();
                let mut artifact_source_digests = Vec::new();
                let mut selected_hunk_count = 0_usize;
                for (((source_path, source_digest), reviewed_plan), diff) in reviewed
                    .source_paths
                    .iter()
                    .zip(&reviewed.artifact_source_digests)
                    .zip(&reviewed.plan.plans)
                    .zip(&reviewed.diffs)
                {
                    let mapping = mappings
                        .iter()
                        .find(|mapping| {
                            mapping.source_path == *source_path
                                && mapping.target_path == reviewed_plan.target_path
                        })
                        .ok_or(WorkspaceProposalError::InvalidRequest)?;
                    if mapping.hunk_ids.is_empty() {
                        continue;
                    }
                    let selected_content = select_workspace_hunks(
                        &reviewed_plan.target_path,
                        reviewed_plan.base_content(),
                        &reviewed_plan.proposed_content,
                        &diff.review_digest,
                        &mapping.hunk_ids,
                    )
                    .map_err(|_| WorkspaceProposalError::InvalidRequest)?;
                    selections.insert(reviewed_plan.target_path.clone(), selected_content);
                    source_paths.push(source_path.clone());
                    artifact_source_digests.push(source_digest.clone());
                    selected_hunk_count += mapping.hunk_ids.len();
                }
                let plan = select_workspace_apply_set(&reviewed.plan, selections)
                    .map_err(map_workspace_prepare_error)?;
                (
                    source_paths,
                    artifact_source_digests,
                    plan,
                    selected_hunk_count,
                )
            } else {
                if mappings.iter().any(|mapping| !mapping.hunk_ids.is_empty()) {
                    return Err(WorkspaceProposalError::InvalidRequest);
                }
                let selected_hunk_count = reviewed.diffs.iter().map(|diff| diff.hunks.len()).sum();
                (
                    reviewed.source_paths,
                    reviewed.artifact_source_digests,
                    reviewed.plan,
                    selected_hunk_count,
                )
            };
        let payload = serde_json::to_vec(&(
            artifact_id,
            artifact_source_revision,
            &source_paths,
            &artifact_source_digests,
            selected_hunk_count,
            &plan,
        ))
        .map_err(|_| WorkspaceProposalError::InvalidRequest)?;
        let payload_digest = sha256_bytes(&payload);
        let mut applies = self
            .workspace_applies
            .lock()
            .map_err(|_| WorkspaceProposalError::Lock)?;
        applies.retain(|_, record| {
            record.session_id != session_id
                || matches!(
                    record.state,
                    WorkspaceApplyState::WaitingApproval | WorkspaceApplyState::Applying
                )
        });
        if applies.values().any(|record| {
            record.session_id == session_id
                && matches!(
                    record.state,
                    WorkspaceApplyState::WaitingApproval | WorkspaceApplyState::Applying
                )
        }) {
            return Err(WorkspaceProposalError::Busy);
        }
        let operation = self
            .config
            .daemon
            .propose_operation(
                session.task_id,
                OperationKind::FileEdit,
                OperationAction::Opaque {
                    kind: "hyper_term.workspace.apply".into(),
                    payload_digest,
                },
                format!(
                    "Apply {} selected hunk(s) across {} Artifact source file(s) from r{} to the workspace",
                    selected_hunk_count,
                    plan.plans.len(),
                    artifact_source_revision
                ),
                RiskClass::WorkspaceWrite,
                vec!["workspace_write".into(), "artifact_apply".into()],
            )
            .map_err(|_| WorkspaceProposalError::Daemon)?;
        let record = WorkspaceApplyRecord {
            session_id,
            task_id: session.task_id,
            artifact_id,
            artifact_source_revision,
            source_paths,
            artifact_source_digests,
            selected_hunk_count,
            waiting_revision: operation.revision,
            plan,
            state: WorkspaceApplyState::WaitingApproval,
        };
        let response =
            workspace_apply_response(operation.operation_id, operation.revision, &record);
        applies.insert(operation.operation_id, record);
        Ok(response)
    }

    pub(super) fn workspace_apply_status(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        operation_id: OperationId,
    ) -> Result<AgentWorkspaceApplyResponse, WorkspaceProposalError> {
        let session = self
            .session(session_id)
            .map_err(|_| WorkspaceProposalError::SessionUnavailable)?;
        let applies = self
            .workspace_applies
            .lock()
            .map_err(|_| WorkspaceProposalError::Lock)?;
        let record = applies
            .get(&operation_id)
            .filter(|record| {
                record.session_id == session_id
                    && record.task_id == session.task_id
                    && record.artifact_id == artifact_id
            })
            .ok_or(WorkspaceProposalError::ArtifactUnavailable)?;
        let revision = self
            .config
            .daemon
            .operation(operation_id)
            .map(|operation| operation.revision)
            .unwrap_or(record.waiting_revision);
        Ok(workspace_apply_response(operation_id, revision, record))
    }

    pub(super) fn execute_workspace_apply(
        &self,
        operation_id: OperationId,
        authorized_revision: u64,
    ) {
        let record = match self
            .workspace_applies
            .lock()
            .ok()
            .and_then(|applies| applies.get(&operation_id).cloned())
        {
            Some(record) => record,
            None => return,
        };
        let dispatching = match self.config.daemon.begin_operation(
            record.task_id,
            operation_id,
            authorized_revision,
        ) {
            Ok(operation) => operation,
            Err(error) => {
                self.set_workspace_apply_failed(operation_id, &error.to_string());
                return;
            }
        };
        let validation: Result<(), String> = (|| {
            let current = self
                .config
                .daemon
                .read_active_genui_artifact(record.task_id, record.artifact_id)
                .map_err(|_| "artifact is no longer current".to_owned())?;
            if current.metadata.source_revision != record.artifact_source_revision {
                return Err("artifact source revision is no longer current".into());
            }
            if record.source_paths.len() != record.plan.plans.len()
                || record.artifact_source_digests.len() != record.plan.plans.len()
            {
                return Err("workspace apply source mapping is inconsistent".into());
            }
            for (source_path, artifact_source_digest) in record
                .source_paths
                .iter()
                .zip(&record.artifact_source_digests)
            {
                let current_source = current
                    .source_files
                    .get(source_path)
                    .ok_or_else(|| "artifact source path is no longer current".to_owned())?;
                if sha256_bytes(current_source.as_bytes()) != *artifact_source_digest {
                    return Err("artifact source digest is no longer current".into());
                }
            }
            Ok(())
        })();
        if let Err(message) = validation {
            let summary = bounded_error(&message);
            let _ = self.config.daemon.complete_operation(
                record.task_id,
                operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "hyper-term-workspace-apply".into(),
                    succeeded: false,
                    outcome: Some(OperationOutcome::Failed),
                    summary: summary.clone(),
                    result_digest: None,
                },
            );
            self.set_workspace_apply_failed(operation_id, &summary);
            return;
        }

        let durable = apply_workspace_set_plan_durable(
            &self.config.workspace,
            &self.config.state_directory,
            WorkspaceTransactionContext {
                task_id: record.task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &record.plan,
        );
        match durable {
            Ok(DurableWorkspaceApplyResult::Committed(receipt)) => {
                self.finish_workspace_transaction(&record, receipt);
            }
            Ok(DurableWorkspaceApplyResult::RolledBack(receipt)) => {
                self.finish_workspace_transaction(&record, receipt);
            }
            Err(error) => {
                let summary = bounded_error(&error.to_string());
                let _ = self.config.daemon.complete_operation(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    OperationCompletion {
                        executor: "hyper-term-workspace-apply".into(),
                        succeeded: false,
                        outcome: Some(OperationOutcome::UnknownExecution),
                        summary: summary.clone(),
                        result_digest: None,
                    },
                );
                self.set_workspace_apply_unknown(operation_id, &summary);
            }
        }
    }

    pub(super) fn finish_workspace_transaction(
        &self,
        record: &WorkspaceApplyRecord,
        receipt: WorkspaceTransactionReceipt,
    ) {
        let committed = receipt.outcome == WorkspaceTransactionOutcome::Committed;
        let summary = if committed {
            format!(
                "applied {} selected hunk(s) across {} Artifact source file(s) to the workspace",
                record.selected_hunk_count,
                record.plan.plans.len(),
            )
        } else {
            bounded_error(
                receipt
                    .failure_summary
                    .as_deref()
                    .unwrap_or("workspace transaction was rolled back"),
            )
        };
        let completion = OperationCompletion {
            executor: "hyper-term-workspace-apply".into(),
            succeeded: committed,
            outcome: Some(if committed {
                OperationOutcome::Succeeded
            } else {
                OperationOutcome::Failed
            }),
            summary: summary.clone(),
            result_digest: committed.then(|| receipt.result_digest.clone()),
        };
        if let Err(error) = self.config.daemon.complete_operation(
            receipt.task_id,
            receipt.operation_id,
            receipt.operation_revision,
            completion,
        ) {
            self.set_workspace_apply_unknown(receipt.operation_id, &error.to_string());
            return;
        }
        if let Err(error) =
            acknowledge_workspace_transaction(&self.config.state_directory, receipt.transaction_id)
        {
            self.set_workspace_apply_unknown(receipt.operation_id, &error.to_string());
            return;
        }
        if committed {
            if let Ok(mut applies) = self.workspace_applies.lock()
                && let Some(record) = applies.get_mut(&receipt.operation_id)
            {
                record.state = WorkspaceApplyState::Applied;
            }
        } else {
            self.set_workspace_apply_failed(receipt.operation_id, &summary);
        }
    }

    pub(super) fn set_workspace_apply_failed(&self, operation_id: OperationId, message: &str) {
        if let Ok(mut applies) = self.workspace_applies.lock()
            && let Some(record) = applies.get_mut(&operation_id)
        {
            record.state = WorkspaceApplyState::Failed(bounded_error(message));
        }
    }

    pub(super) fn set_workspace_apply_unknown(&self, operation_id: OperationId, message: &str) {
        if let Ok(mut applies) = self.workspace_applies.lock()
            && let Some(record) = applies.get_mut(&operation_id)
        {
            record.state = WorkspaceApplyState::UnknownExecution(bounded_error(message));
        }
        if let Ok(mut blocked) = self.workspace_recovery_block.lock() {
            *blocked = Some(bounded_error(message));
        }
    }

    pub(super) fn editor_lsp_query(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: EditorLspRequest,
    ) -> Result<EditorLspResponse, EditorRequestError> {
        let session = self
            .session(session_id)
            .map_err(|_| EditorRequestError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| EditorRequestError::ArtifactUnavailable)?;
        let service = self
            .editor_lsp
            .as_ref()
            .ok_or(EditorRequestError::RuntimeUnavailable)?;
        service
            .query(session_id, &artifact, request)
            .map_err(|error| match error {
                EditorLspError::StaleRevision => EditorRequestError::StaleRevision,
                EditorLspError::InvalidRequest(_) | EditorLspError::DocumentUnavailable => {
                    EditorRequestError::InvalidRequest
                }
                EditorLspError::InvalidRuntime => EditorRequestError::RuntimeUnavailable,
                _ => EditorRequestError::Driver,
            })
    }

    pub(super) fn apply_goal_command(
        &self,
        session_id: u16,
        command: &str,
    ) -> Result<AgentTurnResponse, SessionError> {
        let session = self.session(session_id)?;
        {
            let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            if matches!(
                progress.status,
                AgentStatus::Running | AgentStatus::Cancelling | AgentStatus::WaitingApproval
            ) {
                return Err(SessionError::Busy);
            }
        }
        let argument = command
            .trim()
            .strip_prefix("/goal")
            .ok_or(SessionError::InvalidConfig)?
            .trim();
        if argument.eq_ignore_ascii_case("clear") {
            session
                .client
                .clear_thread_goal(&session.thread_id, START_TURN_TIMEOUT)
                .map_err(|error| match error {
                    hyper_term_drivers::AgentClientError::Unsupported(_) => {
                        SessionError::Unsupported
                    }
                    _ => SessionError::Driver,
                })?;
        } else if !argument.is_empty() {
            let (objective, status) = match argument {
                "pause" => (None, Some(AgentGoalStatus::Paused)),
                "resume" => (None, Some(AgentGoalStatus::Active)),
                _ => (Some(argument), Some(AgentGoalStatus::Active)),
            };
            session
                .client
                .set_thread_goal(&session.thread_id, objective, status, START_TURN_TIMEOUT)
                .map_err(|error| match error {
                    hyper_term_drivers::AgentClientError::Unsupported(_) => {
                        SessionError::Unsupported
                    }
                    _ => SessionError::Driver,
                })?;
        }
        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Ready,
        })
    }

    pub(super) fn submit_turn(
        &self,
        session_id: u16,
        prompt: String,
    ) -> Result<AgentTurnResponse, SessionError> {
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(SessionError::PromptTooLarge);
        }
        let session = self.session(session_id)?;
        {
            let mut progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            if matches!(
                progress.status,
                AgentStatus::Running | AgentStatus::Cancelling | AgentStatus::WaitingApproval
            ) {
                return Err(SessionError::Busy);
            }
            progress.status = AgentStatus::Running;
            progress.turn_id = None;
            progress.error = None;
        }
        self.config
            .daemon
            .append_message(
                session.task_id,
                BlockId::new(),
                MessageRole::User,
                None,
                prompt.clone(),
            )
            .map_err(|_| SessionError::Daemon)?;
        let daemon = self.config.daemon.clone();
        let worker_session = Arc::clone(&session);
        std::thread::Builder::new()
            .name(format!("hyper-term-agent-{session_id}"))
            .spawn(move || run_turn(worker_session, daemon, prompt))
            .map_err(|_| {
                set_progress_failed(&session, "Agent turn worker could not start");
                SessionError::Thread
            })?;
        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Running,
        })
    }

    pub(super) fn cancel_turn(&self, session_id: u16) -> Result<AgentTurnResponse, SessionError> {
        let session = self.session(session_id)?;
        let (turn_id, waiting_approval) = {
            let mut progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            match progress.status {
                AgentStatus::Cancelling => {
                    return Ok(AgentTurnResponse {
                        session_id,
                        status: AgentStatus::Cancelling,
                    });
                }
                AgentStatus::Running | AgentStatus::WaitingApproval => {}
                _ => return Err(SessionError::NoActiveTurn),
            }
            let waiting_approval = progress.status == AgentStatus::WaitingApproval;
            progress.status = AgentStatus::Cancelling;
            progress.error = None;
            (progress.turn_id.clone(), waiting_approval)
        };

        let pending = session
            .pending_effect
            .lock()
            .map_err(|_| SessionError::Lock)?
            .take();
        let projection = if let Some(effect) = pending {
            let decided = self
                .config
                .daemon
                .decide_permission(
                    session.task_id,
                    effect.operation_id,
                    effect.operation_revision,
                    PermissionDecision::Cancelled,
                )
                .map_err(|_| SessionError::StalePermission)?;
            if let Some(host_request) = &effect.host_request {
                session
                    .client
                    .resolve_host_request(
                        &host_request.request_id,
                        AgentHostResponse::Error {
                            code: -32800,
                            message: "Agent turn cancelled by user".into(),
                        },
                    )
                    .map_err(|_| SessionError::Driver)?;
            } else {
                session
                    .client
                    .resolve_effect(
                        &effect.request_id,
                        AgentEffectAuthorization {
                            operation_id: effect.operation_id,
                            operation_revision: decided.revision,
                            proposal_sha256: effect.payload_sha256,
                            decision: PermissionDecision::Cancelled,
                        },
                    )
                    .map_err(|_| SessionError::Driver)?;
            }
            Some(effect.projection)
        } else {
            None
        };

        if let Some(turn_id) = turn_id.as_deref()
            && session
                .client
                .cancel_turn(&session.thread_id, turn_id)
                .is_err()
        {
            set_progress_failed(&session, "Agent turn cancellation could not be delivered");
            return Err(SessionError::Driver);
        }

        if waiting_approval && projection.is_none() {
            return Err(SessionError::NoPendingEffect);
        }
        if let Some(projection) = projection {
            let daemon = self.config.daemon.clone();
            let worker_session = Arc::clone(&session);
            std::thread::Builder::new()
                .name(format!("hyper-term-agent-{session_id}-cancel"))
                .spawn(move || continue_turn(worker_session, daemon, projection))
                .map_err(|_| {
                    set_progress_failed(&session, "Agent cancellation worker could not start");
                    SessionError::Thread
                })?;
        }

        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Cancelling,
        })
    }

    pub(super) fn set_session_config(
        &self,
        session_id: u16,
        request: AgentConfigRequest,
    ) -> Result<AgentCapabilitiesResponse, SessionError> {
        let session = self.session(session_id)?;
        if session.protocol != StructuredAgentProtocol::Acp {
            return Err(SessionError::Unsupported);
        }
        {
            let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
            if matches!(
                progress.status,
                AgentStatus::Running | AgentStatus::Cancelling | AgentStatus::WaitingApproval
            ) {
                return Err(SessionError::Busy);
            }
        }
        let capabilities = session
            .client
            .set_session_config_option(
                &session.thread_id,
                &request.config_id,
                request.value,
                START_TURN_TIMEOUT,
            )
            .map_err(|error| match error {
                hyper_term_drivers::AgentClientError::Acp(
                    hyper_term_drivers::AcpAdapterError::InvalidMessage(_),
                ) => SessionError::InvalidConfig,
                hyper_term_drivers::AgentClientError::Unsupported(_) => SessionError::Unsupported,
                _ => SessionError::Driver,
            })?;
        Ok(AgentCapabilitiesResponse {
            session_id,
            capabilities,
        })
    }

    pub(super) fn tier2_results(
        &self,
        session_id: u16,
    ) -> Result<AgentTier2ResultsResponse, SessionError> {
        let session = self.session(session_id)?;
        let acceptances = self
            .config
            .daemon
            .isolated_acceptance_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .into_iter()
            .map(|review| (review.source_operation_id, tier2_review_response(review)))
            .collect::<HashMap<_, _>>();
        let results = self
            .config
            .daemon
            .isolated_result_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .into_iter()
            .map(|review| AgentTier2ResultResponse {
                source_operation_id: review.operation_id,
                source_revision: review.receipt.source_revision,
                finished_at_ms: review.receipt.finished_at_ms,
                termination: review.receipt.termination,
                exit_code: review.receipt.exit_code,
                changed_bytes: review.receipt.changes.changed_bytes,
                inventory_sha256: review.receipt.changes.inventory_sha256,
                changed_files: review.receipt.changes.changed_files,
                acceptance: acceptances.get(&review.operation_id).cloned(),
            })
            .collect();
        Ok(AgentTier2ResultsResponse { results })
    }

    pub(super) fn preview_tier2_result(
        &self,
        session_id: u16,
        source_operation_id: OperationId,
    ) -> Result<AgentTier2PreviewResponse, SessionError> {
        let session = self.session(session_id)?;
        let preview = self
            .config
            .daemon
            .preview_isolated_result_acceptance(session.task_id, source_operation_id)
            .map_err(|error| match error {
                DaemonError::IsolatedResultMissing(_) => SessionError::NotFound,
                _ => SessionError::Daemon,
            })?;
        Ok(tier2_preview_response(preview))
    }

    pub(super) fn propose_tier2_review(
        &self,
        session_id: u16,
        source_operation_id: OperationId,
    ) -> Result<AgentTier2ReviewResponse, SessionError> {
        let session = self.session(session_id)?;
        if !self
            .config
            .daemon
            .isolated_result_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .iter()
            .any(|result| result.operation_id == source_operation_id)
        {
            return Err(SessionError::NotFound);
        }
        let review = self
            .config
            .daemon
            .propose_isolated_result_acceptance(session.task_id, source_operation_id)
            .map_err(|error| match error {
                DaemonError::IsolatedAcceptanceAlreadyExists(_) => SessionError::Busy,
                DaemonError::IsolatedResultMissing(_) => SessionError::NotFound,
                _ => SessionError::Daemon,
            })?;
        Ok(tier2_review_response(review))
    }

    pub(super) fn discard_tier2_result(
        &self,
        session_id: u16,
        source_operation_id: OperationId,
    ) -> Result<(), SessionError> {
        let session = self.session(session_id)?;
        if !self
            .config
            .daemon
            .isolated_result_reviews(session.task_id)
            .map_err(|_| SessionError::Daemon)?
            .iter()
            .any(|result| result.operation_id == source_operation_id)
        {
            return Err(SessionError::NotFound);
        }
        self.config
            .daemon
            .discard_isolated_result(source_operation_id)
            .map_err(|error| match error {
                DaemonError::IsolatedResultHasPendingAcceptance(_) => SessionError::Busy,
                DaemonError::IsolatedResultMissing(_) => SessionError::NotFound,
                _ => SessionError::Daemon,
            })
    }

    pub(super) fn decide_effect(
        &self,
        session_id: u16,
        request: AgentPermissionRequest,
    ) -> Result<AgentTurnResponse, SessionError> {
        if !matches!(
            request.decision,
            PermissionDecision::AllowOnce
                | PermissionDecision::RejectOnce
                | PermissionDecision::Cancelled
        ) {
            return Err(SessionError::UnsafeApproval);
        }
        let session = self.session(session_id)?;
        let mut pending = session
            .pending_effect
            .lock()
            .map_err(|_| SessionError::Lock)?;
        let effect = pending
            .as_ref()
            .filter(|effect| {
                effect.operation_id == request.operation_id
                    && effect.operation_revision == request.expected_revision
            })
            .cloned();
        if effect.is_none() {
            drop(pending);
            let operation = self
                .config
                .daemon
                .operation(request.operation_id)
                .map_err(|_| SessionError::NoPendingEffect)?;
            if operation.task_id != session.task_id
                || operation.revision != request.expected_revision
                || operation.state != hyper_term_protocol::OperationState::WaitingHuman
            {
                return Err(SessionError::StalePermission);
            }
            let draft = self
                .artifact_drafts
                .lock()
                .map_err(|_| SessionError::Lock)?
                .get(&request.operation_id)
                .cloned();
            let workspace_apply = self
                .workspace_applies
                .lock()
                .map_err(|_| SessionError::Lock)?
                .get(&request.operation_id)
                .cloned();
            let tier2_review = match self
                .config
                .daemon
                .isolated_acceptance_review(request.operation_id)
            {
                Ok(review) => Some(review),
                Err(DaemonError::IsolatedAcceptanceMissing(_)) => None,
                Err(_) => return Err(SessionError::Daemon),
            };
            if request.decision == PermissionDecision::AllowOnce
                && workspace_apply.is_none()
                && tier2_review.is_none()
                && !allowable_brokered_mcp_operation(&operation)
            {
                return Err(SessionError::UnsafeApproval);
            }
            if let Some(draft) = draft {
                if draft.session_id != session_id
                    || draft.task_id != session.task_id
                    || draft.waiting_revision != request.expected_revision
                    || !matches!(draft.state, ArtifactDraftState::WaitingApproval)
                {
                    return Err(SessionError::StalePermission);
                }
                let decided = self
                    .config
                    .daemon
                    .decide_permission(
                        session.task_id,
                        request.operation_id,
                        request.expected_revision,
                        request.decision,
                    )
                    .map_err(|_| SessionError::StalePermission)?;
                {
                    let mut drafts = self
                        .artifact_drafts
                        .lock()
                        .map_err(|_| SessionError::Lock)?;
                    let record = drafts
                        .get_mut(&request.operation_id)
                        .ok_or(SessionError::StalePermission)?;
                    record.state = if request.decision == PermissionDecision::AllowOnce {
                        ArtifactDraftState::Compiling
                    } else {
                        ArtifactDraftState::Rejected
                    };
                }
                if request.decision == PermissionDecision::AllowOnce {
                    let runtime = self.clone();
                    std::thread::Builder::new()
                        .name(format!("hyper-term-artifact-draft-{session_id}"))
                        .spawn(move || {
                            runtime.execute_artifact_draft(request.operation_id, decided.revision)
                        })
                        .map_err(|_| {
                            self.set_artifact_draft_failed(
                                request.operation_id,
                                "Artifact draft worker could not start",
                            );
                            SessionError::Thread
                        })?;
                }
                let status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                return Ok(AgentTurnResponse { session_id, status });
            }
            if let Some(workspace_apply) = workspace_apply {
                if workspace_apply.session_id != session_id
                    || workspace_apply.task_id != session.task_id
                    || workspace_apply.waiting_revision != request.expected_revision
                    || !matches!(workspace_apply.state, WorkspaceApplyState::WaitingApproval)
                    || operation.kind != OperationKind::FileEdit
                    || operation.risk != RiskClass::WorkspaceWrite
                    || !matches!(
                        &operation.action,
                        OperationAction::Opaque { kind, .. }
                            if kind == "hyper_term.workspace.apply"
                    )
                {
                    return Err(SessionError::StalePermission);
                }
                let decided = self
                    .config
                    .daemon
                    .decide_permission(
                        session.task_id,
                        request.operation_id,
                        request.expected_revision,
                        request.decision,
                    )
                    .map_err(|_| SessionError::StalePermission)?;
                {
                    let mut applies = self
                        .workspace_applies
                        .lock()
                        .map_err(|_| SessionError::Lock)?;
                    let record = applies
                        .get_mut(&request.operation_id)
                        .ok_or(SessionError::StalePermission)?;
                    record.state = if request.decision == PermissionDecision::AllowOnce {
                        WorkspaceApplyState::Applying
                    } else {
                        WorkspaceApplyState::Rejected
                    };
                }
                if request.decision == PermissionDecision::AllowOnce {
                    let runtime = self.clone();
                    std::thread::Builder::new()
                        .name(format!("hyper-term-workspace-apply-{session_id}"))
                        .spawn(move || {
                            runtime.execute_workspace_apply(request.operation_id, decided.revision)
                        })
                        .map_err(|_| {
                            self.set_workspace_apply_failed(
                                request.operation_id,
                                "Workspace apply worker could not start",
                            );
                            SessionError::Thread
                        })?;
                }
                let status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                return Ok(AgentTurnResponse { session_id, status });
            }
            if let Some(review) = tier2_review {
                if review.operation.task_id != session.task_id
                    || review.operation.revision != request.expected_revision
                    || review.operation.state != OperationState::WaitingHuman
                    || review.operation.kind != OperationKind::FileEdit
                    || review.operation.risk != RiskClass::WorkspaceWrite
                    || !matches!(
                        &review.operation.action,
                        OperationAction::Opaque { kind, .. }
                            if kind == "hyper_term.tier2.accept"
                    )
                {
                    return Err(SessionError::StalePermission);
                }
                let decided = self
                    .config
                    .daemon
                    .decide_isolated_acceptance_permission(
                        session.task_id,
                        request.operation_id,
                        request.expected_revision,
                        request.decision,
                    )
                    .map_err(|_| SessionError::StalePermission)?;
                if request.decision == PermissionDecision::AllowOnce {
                    self.config
                        .daemon
                        .accept_isolated_result(
                            session.task_id,
                            request.operation_id,
                            decided.revision,
                        )
                        .map_err(|_| SessionError::Daemon)?;
                }
                let status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                return Ok(AgentTurnResponse { session_id, status });
            }
            self.config
                .daemon
                .decide_permission(
                    session.task_id,
                    request.operation_id,
                    request.expected_revision,
                    request.decision,
                )
                .map_err(|_| SessionError::StalePermission)?;
            let status = session
                .progress
                .lock()
                .map_err(|_| SessionError::Lock)?
                .status;
            return Ok(AgentTurnResponse { session_id, status });
        }
        let effect = effect.expect("checked pending effect");
        if let Some(host_request) = effect.host_request.clone() {
            let AgentHostOperation::TerminalCreate { .. } = host_request.operation else {
                return Err(SessionError::Driver);
            };
            let runner = if request.decision == PermissionDecision::AllowOnce {
                Some(
                    self.config
                        .tier2_runner
                        .clone()
                        .ok_or(SessionError::UnsafeApproval)?,
                )
            } else {
                None
            };
            let decided = self
                .config
                .daemon
                .decide_permission(
                    session.task_id,
                    effect.operation_id,
                    effect.operation_revision,
                    request.decision,
                )
                .map_err(|_| SessionError::StalePermission)?;
            pending.take();
            drop(pending);
            if let Ok(mut progress) = session.progress.lock() {
                progress.status = AgentStatus::Running;
                progress.error = None;
            } else {
                let _ = session.client.close();
                return Err(SessionError::Lock);
            }
            let projection = effect.projection;
            let daemon = self.config.daemon.clone();
            let worker_session = Arc::clone(&session);
            if let Some(runner) = runner {
                std::thread::Builder::new()
                    .name(format!("hyper-term-agent-{session_id}-terminal"))
                    .spawn(move || {
                        execute_agent_terminal_create(
                            worker_session,
                            daemon,
                            runner,
                            host_request,
                            effect.operation_id,
                            decided.revision,
                            projection,
                        )
                    })
                    .map_err(|_| {
                        set_progress_failed(&session, "ACP terminal worker could not start");
                        SessionError::Thread
                    })?;
            } else {
                if session
                    .client
                    .resolve_host_request(
                        &host_request.request_id,
                        AgentHostResponse::Error {
                            code: -32000,
                            message: "ACP terminal request was not approved".into(),
                        },
                    )
                    .is_err()
                {
                    set_progress_failed(&session, "ACP terminal decision could not be returned");
                    let _ = session.client.close();
                    return Err(SessionError::Driver);
                }
                std::thread::Builder::new()
                    .name(format!("hyper-term-agent-{session_id}-resume"))
                    .spawn(move || continue_turn(worker_session, daemon, projection))
                    .map_err(|_| {
                        set_progress_failed(&session, "Agent turn resume worker could not start");
                        SessionError::Thread
                    })?;
            }
            return Ok(AgentTurnResponse {
                session_id,
                status: AgentStatus::Running,
            });
        }
        if request.decision == PermissionDecision::AllowOnce {
            return Err(SessionError::UnsafeApproval);
        }
        let decided = self
            .config
            .daemon
            .decide_permission(
                session.task_id,
                effect.operation_id,
                effect.operation_revision,
                request.decision,
            )
            .map_err(|_| SessionError::StalePermission)?;
        if session
            .client
            .resolve_effect(
                &effect.request_id,
                AgentEffectAuthorization {
                    operation_id: effect.operation_id,
                    operation_revision: decided.revision,
                    proposal_sha256: effect.payload_sha256,
                    decision: request.decision,
                },
            )
            .is_err()
        {
            set_progress_failed(&session, "Agent effect decision could not be returned");
            let _ = session.client.close();
            return Err(SessionError::Driver);
        }
        pending.take();
        drop(pending);
        if let Ok(mut progress) = session.progress.lock() {
            progress.status = AgentStatus::Running;
            progress.error = None;
        } else {
            let _ = session.client.close();
            return Err(SessionError::Lock);
        }
        let daemon = self.config.daemon.clone();
        let projection = effect.projection;
        let worker_session = Arc::clone(&session);
        std::thread::Builder::new()
            .name(format!("hyper-term-agent-{session_id}-resume"))
            .spawn(move || continue_turn(worker_session, daemon, projection))
            .map_err(|_| {
                set_progress_failed(&session, "Agent turn resume worker could not start");
                SessionError::Thread
            })?;
        Ok(AgentTurnResponse {
            session_id,
            status: AgentStatus::Running,
        })
    }

    pub(super) fn session(&self, session_id: u16) -> Result<Arc<AgentSession>, SessionError> {
        self.sessions
            .lock()
            .map_err(|_| SessionError::Lock)?
            .get(&session_id)
            .cloned()
            .ok_or(SessionError::NotFound)
    }

    pub(super) fn close_session(
        &self,
        session_id: u16,
        forget_history: bool,
    ) -> Result<Option<TaskId>, SessionError> {
        if forget_history {
            self.session_bindings
                .forget(session_id)
                .map_err(|_| SessionError::Daemon)?;
        }
        self.close_artifact_drafts(session_id);
        self.close_workspace_applies(session_id);
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id));
        if let Some(session) = session {
            let task_id = session.task_id;
            if let Some(capability_server) = &session.capability_server {
                capability_server.revoke();
            }
            let _ = session.client.close();
            self.cleanup_brokered_mcp_runtime(session.task_id);
            let _ = std::fs::remove_dir_all(&session.runtime_root);
            if let Some(editor_lsp) = &self.editor_lsp {
                editor_lsp.close_session(session_id);
            }
            return Ok(Some(task_id));
        }
        if let Some(editor_lsp) = &self.editor_lsp {
            editor_lsp.close_session(session_id);
        }
        Ok(None)
    }

    pub(super) fn close_all(&self) {
        let session_ids = self
            .sessions
            .lock()
            .map(|sessions| sessions.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for session_id in session_ids {
            let _ = self.close_session(session_id, false);
        }
        if let Some(editor_lsp) = &self.editor_lsp {
            editor_lsp.close_all();
        }
        if let Some(compiler) = &self.artifact_draft_compiler {
            compiler.close();
        }
    }

    pub(super) fn brokered_mcp_root(&self, task_id: TaskId) -> PathBuf {
        self.config
            .state_directory
            .join("brokered-mcp")
            .join(task_id.to_string())
    }

    pub(super) fn cleanup_brokered_mcp_runtime(&self, task_id: TaskId) {
        let _ = self.config.daemon.unregister_brokered_mcp_runtime(task_id);
        let _ = std::fs::remove_dir_all(self.brokered_mcp_root(task_id));
    }

    pub(super) fn close_artifact_drafts(&self, session_id: u16) {
        let waiting = self
            .artifact_drafts
            .lock()
            .map(|drafts| {
                drafts
                    .iter()
                    .filter_map(|(operation_id, record)| {
                        (record.session_id == session_id
                            && matches!(record.state, ArtifactDraftState::WaitingApproval))
                        .then_some((*operation_id, record.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (operation_id, record) in waiting {
            let _ = self.config.daemon.decide_permission(
                record.task_id,
                operation_id,
                record.waiting_revision,
                PermissionDecision::Cancelled,
            );
        }
        if let Ok(mut drafts) = self.artifact_drafts.lock() {
            drafts.retain(|_, record| record.session_id != session_id);
        }
    }

    pub(super) fn close_workspace_applies(&self, session_id: u16) {
        let waiting = self
            .workspace_applies
            .lock()
            .map(|applies| {
                applies
                    .iter()
                    .filter_map(|(operation_id, record)| {
                        (record.session_id == session_id
                            && matches!(record.state, WorkspaceApplyState::WaitingApproval))
                        .then_some((*operation_id, record.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (operation_id, record) in waiting {
            let _ = self.config.daemon.decide_permission(
                record.task_id,
                operation_id,
                record.waiting_revision,
                PermissionDecision::Cancelled,
            );
        }
        if let Ok(mut applies) = self.workspace_applies.lock() {
            applies.retain(|_, record| record.session_id != session_id);
        }
    }
}
