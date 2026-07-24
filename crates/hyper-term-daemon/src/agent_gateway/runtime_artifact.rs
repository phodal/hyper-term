use super::*;

impl AgentGatewayRuntime {
    pub(super) fn preview_document(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<String, SessionError> {
        let session = self.session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?;
        let shell = self
            .preview_shell
            .as_deref()
            .ok_or(SessionError::ArtifactUnavailable)?;
        render_preview_document(shell, &artifact).map_err(|_| SessionError::ArtifactUnavailable)
    }

    pub(super) fn artifact_source_map(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<String, SessionError> {
        let session = self.session(session_id)?;
        self.config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map(|artifact| artifact.source_map)
            .map_err(|_| SessionError::ArtifactUnavailable)
    }

    pub(super) fn artifact_source(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<AgentArtifactSourceResponse, SessionError> {
        let session = self.session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?;
        if artifact.source_files.is_empty() {
            return Err(SessionError::ArtifactUnavailable);
        }
        Ok(AgentArtifactSourceResponse {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            entrypoint: artifact.metadata.entrypoint,
            files: artifact.source_files,
        })
    }

    pub(super) fn artifact_editor_state(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<ArtifactEditorCheckpoint, ArtifactEditorError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactEditorError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| ArtifactEditorError::ArtifactUnavailable)?;
        let _guard = self
            .artifact_editor_lock
            .lock()
            .map_err(|_| ArtifactEditorError::Lock)?;
        self.artifact_editor_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
                &artifact.metadata.entrypoint,
                &artifact.source_files,
            )
            .map_err(map_artifact_editor_store_error)
    }

    pub(super) fn save_artifact_editor_state(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: ArtifactEditorCheckpointRequest,
    ) -> Result<ArtifactEditorCheckpoint, ArtifactEditorError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactEditorError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| ArtifactEditorError::ArtifactUnavailable)?;
        if request.base_source_revision != artifact.metadata.source_revision {
            return Err(ArtifactEditorError::StaleRevision);
        }
        let _guard = self
            .artifact_editor_lock
            .lock()
            .map_err(|_| ArtifactEditorError::Lock)?;
        self.artifact_editor_store
            .save(
                session.task_id,
                artifact_id,
                &artifact.metadata.entrypoint,
                &artifact.source_files,
                request,
            )
            .map_err(map_artifact_editor_store_error)
    }

    pub(super) fn artifact_runtime_trace(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<GenUiRuntimeTraceProjection, RuntimeTraceError> {
        let session = self
            .session(session_id)
            .map_err(|_| RuntimeTraceError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| RuntimeTraceError::ArtifactUnavailable)?;
        let _guard = self
            .artifact_runtime_trace_lock
            .lock()
            .map_err(|_| RuntimeTraceError::Lock)?;
        self.artifact_runtime_trace_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
            )
            .map_err(map_runtime_trace_store_error)
    }

    pub(super) fn append_artifact_runtime_trace(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        request: GenUiRuntimeTraceAppendRequest,
    ) -> Result<GenUiRuntimeTraceProjection, RuntimeTraceError> {
        let session = self
            .session(session_id)
            .map_err(|_| RuntimeTraceError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| RuntimeTraceError::ArtifactUnavailable)?;
        if request.source_revision != artifact.metadata.source_revision {
            return Err(RuntimeTraceError::StaleRevision);
        }
        let _guard = self
            .artifact_runtime_trace_lock
            .lock()
            .map_err(|_| RuntimeTraceError::Lock)?;
        self.artifact_runtime_trace_store
            .append(
                session.task_id,
                artifact_id,
                request.source_revision,
                request.events,
            )
            .map_err(map_runtime_trace_store_error)
    }

    pub(super) fn artifact_debug_capsule(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
    ) -> Result<GenUiBugCapsule, BugCapsuleRequestError> {
        let session = self
            .session(session_id)
            .map_err(|_| BugCapsuleRequestError::SessionUnavailable)?;
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| BugCapsuleRequestError::ArtifactUnavailable)?;
        let _editor_guard = self
            .artifact_editor_lock
            .lock()
            .map_err(|_| BugCapsuleRequestError::Lock)?;
        let editor = self
            .artifact_editor_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
                &artifact.metadata.entrypoint,
                &artifact.source_files,
            )
            .map_err(|_| BugCapsuleRequestError::Store)?;
        let _trace_guard = self
            .artifact_runtime_trace_lock
            .lock()
            .map_err(|_| BugCapsuleRequestError::Lock)?;
        let runtime = self
            .artifact_runtime_trace_store
            .load(
                session.task_id,
                artifact_id,
                artifact.metadata.source_revision,
            )
            .map_err(|_| BugCapsuleRequestError::Store)?;
        let compiler = self.artifact_draft_compiler.as_deref();
        let environment = GenUiBugCapsuleEnvironment {
            hyper_term_version: env!("CARGO_PKG_VERSION").into(),
            os: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            deno_runtime_version: compiler.map(|compiler| compiler.config.runtime_version.clone()),
            deno_executable_digest: compiler
                .map(|compiler| compiler.config.executable_sha256.clone()),
            compiler_script_digest: compiler
                .map(|compiler| compiler.config.compiler_script_sha256.clone()),
            compiler_wasm_digest: compiler
                .map(|compiler| compiler.config.compiler_wasm_sha256.clone()),
        };
        build_bug_capsule(&artifact, &editor, &runtime, environment)
            .map_err(|_| BugCapsuleRequestError::Store)
    }

    pub(super) fn artifact_history(
        &self,
        session_id: u16,
        active_artifact_id: ArtifactId,
    ) -> Result<AgentArtifactHistoryResponse, SessionError> {
        let session = self.session(session_id)?;
        let entries = self
            .config
            .daemon
            .genui_artifact_history(session.task_id, active_artifact_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?
            .into_iter()
            .map(|entry| AgentArtifactHistoryEntry {
                event_sequence: entry.event_sequence,
                recorded_at_ms: entry.recorded_at_ms,
                operation_id: entry.operation_id,
                artifact: entry.artifact,
            })
            .collect();
        Ok(AgentArtifactHistoryResponse {
            active_artifact_id,
            entries,
        })
    }

    pub(super) fn artifact_history_source(
        &self,
        session_id: u16,
        active_artifact_id: ArtifactId,
        revision_id: ArtifactId,
    ) -> Result<AgentArtifactSourceResponse, SessionError> {
        let session = self.session(session_id)?;
        let artifact = self
            .config
            .daemon
            .read_genui_artifact_revision(session.task_id, active_artifact_id, revision_id)
            .map_err(|_| SessionError::ArtifactUnavailable)?;
        if artifact.source_files.is_empty() {
            return Err(SessionError::ArtifactUnavailable);
        }
        Ok(AgentArtifactSourceResponse {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            entrypoint: artifact.metadata.entrypoint,
            files: artifact.source_files,
        })
    }

    pub(super) fn propose_artifact_draft(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        draft: AgentArtifactDraftRequest,
    ) -> Result<AgentArtifactDraftResponse, ArtifactDraftError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactDraftError::SessionUnavailable)?;
        if self.artifact_draft_compiler.is_none() {
            return Err(ArtifactDraftError::RuntimeUnavailable);
        }
        let artifact = self
            .config
            .daemon
            .read_active_genui_artifact(session.task_id, artifact_id)
            .map_err(|_| ArtifactDraftError::ArtifactUnavailable)?;
        let request = validate_artifact_draft(&artifact, draft)?;
        let payload =
            serde_json::to_vec(&request).map_err(|_| ArtifactDraftError::InvalidRequest)?;
        let payload_digest = sha256_bytes(&payload);
        let mut drafts = self
            .artifact_drafts
            .lock()
            .map_err(|_| ArtifactDraftError::Lock)?;
        drafts.retain(|_, record| {
            record.session_id != session_id
                || matches!(
                    record.state,
                    ArtifactDraftState::WaitingApproval | ArtifactDraftState::Compiling
                )
        });
        if drafts.values().any(|record| {
            record.session_id == session_id
                && matches!(
                    record.state,
                    ArtifactDraftState::WaitingApproval | ArtifactDraftState::Compiling
                )
        }) {
            return Err(ArtifactDraftError::Busy);
        }
        let operation = self
            .config
            .daemon
            .propose_operation(
                session.task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest,
                },
                format!(
                    "Publish edited GenUI artifact revision {}",
                    request.source_revision
                ),
                RiskClass::ReadOnly,
                vec![
                    "artifact_build".into(),
                    "deno_runtime".into(),
                    "artifact_publish".into(),
                ],
            )
            .map_err(|_| ArtifactDraftError::Daemon)?;
        let record = ArtifactDraftRecord {
            session_id,
            task_id: session.task_id,
            base_artifact_id: artifact_id,
            base_source_revision: artifact.metadata.source_revision,
            waiting_revision: operation.revision,
            request,
            state: ArtifactDraftState::WaitingApproval,
        };
        let response = artifact_draft_response(operation.operation_id, operation.revision, &record);
        drafts.insert(operation.operation_id, record);
        Ok(response)
    }

    pub(super) fn artifact_draft_status(
        &self,
        session_id: u16,
        artifact_id: ArtifactId,
        operation_id: OperationId,
    ) -> Result<AgentArtifactDraftResponse, ArtifactDraftError> {
        let session = self
            .session(session_id)
            .map_err(|_| ArtifactDraftError::SessionUnavailable)?;
        let drafts = self
            .artifact_drafts
            .lock()
            .map_err(|_| ArtifactDraftError::Lock)?;
        let record = drafts
            .get(&operation_id)
            .filter(|record| {
                record.session_id == session_id
                    && record.task_id == session.task_id
                    && record.base_artifact_id == artifact_id
            })
            .ok_or(ArtifactDraftError::ArtifactUnavailable)?;
        let revision = self
            .config
            .daemon
            .operation(operation_id)
            .map(|operation| operation.revision)
            .unwrap_or(record.waiting_revision);
        Ok(artifact_draft_response(operation_id, revision, record))
    }

    pub(super) fn execute_artifact_draft(
        &self,
        operation_id: OperationId,
        authorized_revision: u64,
    ) {
        let record = match self
            .artifact_drafts
            .lock()
            .ok()
            .and_then(|drafts| drafts.get(&operation_id).cloned())
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
                self.set_artifact_draft_failed(operation_id, &error.to_string());
                return;
            }
        };
        let result = (|| {
            let current = self
                .config
                .daemon
                .read_active_genui_artifact(record.task_id, record.base_artifact_id)
                .map_err(|_| "base artifact is no longer current".to_owned())?;
            if current.metadata.source_revision != record.base_source_revision {
                return Err("base artifact revision is no longer current".to_owned());
            }
            let compiler = self
                .artifact_draft_compiler
                .as_ref()
                .ok_or_else(|| "Rust-supervised Deno compiler is unavailable".to_owned())?;
            let candidate = compiler.compile(record.request.clone())?;
            self.config
                .daemon
                .accept_genui_artifact_from_base(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    record.base_artifact_id,
                    record.base_source_revision,
                    candidate,
                )
                .map_err(|error| error.to_string())
        })();
        match result {
            Ok(artifact) => {
                let completion = OperationCompletion {
                    executor: "hyper-term-artifact-draft".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: format!(
                        "published GenUI artifact revision {}",
                        artifact.source_revision
                    ),
                    result_digest: Some(artifact.content_digest.clone()),
                };
                if let Err(error) = self.config.daemon.complete_operation(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    completion,
                ) {
                    self.set_artifact_draft_failed(operation_id, &error.to_string());
                    return;
                }
                if let Ok(mut drafts) = self.artifact_drafts.lock()
                    && let Some(record) = drafts.get_mut(&operation_id)
                {
                    record.state = ArtifactDraftState::Accepted(artifact);
                }
            }
            Err(message) => {
                let summary = bounded_error(&message);
                let _ = self.config.daemon.complete_operation(
                    record.task_id,
                    operation_id,
                    dispatching.revision,
                    OperationCompletion {
                        executor: "hyper-term-artifact-draft".into(),
                        succeeded: false,
                        outcome: Some(OperationOutcome::Failed),
                        summary: summary.clone(),
                        result_digest: None,
                    },
                );
                self.set_artifact_draft_failed(operation_id, &summary);
            }
        }
    }

    pub(super) fn set_artifact_draft_failed(&self, operation_id: OperationId, message: &str) {
        if let Ok(mut drafts) = self.artifact_drafts.lock()
            && let Some(record) = drafts.get_mut(&operation_id)
        {
            record.state = ArtifactDraftState::Failed(bounded_error(message));
        }
    }
}
