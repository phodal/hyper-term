use super::*;

impl AgentGatewayRuntime {
    pub(super) fn start_agent(
        &self,
        session_id: u16,
        provider_id: &str,
    ) -> Result<AgentSessionResponse, StartError> {
        let mut sessions = self.sessions.lock().map_err(|_| StartError::Lock)?;
        if let Some(session) = sessions.get(&session_id) {
            if session.provider_id != provider_id {
                return Err(StartError::ProviderMismatch);
            }
            return match session.client.state().map_err(|_| StartError::Driver)? {
                DriverState::Ready | DriverState::Busy | DriverState::Waiting => {
                    Ok(ready_response(session_id, session))
                }
                _ => Err(StartError::Driver),
            };
        }
        if sessions.len() >= MAX_AGENT_SESSIONS {
            return Err(StartError::Capacity);
        }
        // A staged Codex auth candidate is the desktop contract and must pass
        // the read-only readiness gate. Library callers that intentionally do
        // not stage credentials may still let app-server negotiate its own
        // authentication. ACP registrations always use the explicit provider
        // readiness contract.
        let enforce_readiness = provider_id != "codex" || self.config.codex_auth_file.is_some();
        if enforce_readiness
            && let Some(status) = probe_known_agent_provider(&self.config, provider_id)
            && !status.usable()
        {
            return Err(StartError::Unavailable);
        }
        let restored_task_id = self
            .session_bindings
            .task_for(session_id, provider_id)
            .map_err(|_| StartError::Driver)?
            .filter(|task_id| self.config.daemon.block_snapshot(*task_id).is_ok());
        let task_id = match restored_task_id {
            Some(task_id) => task_id,
            None => self
                .config
                .daemon
                .create_task(format!("{provider_id} Agent session {session_id}"))
                .map_err(|_| StartError::Driver)?,
        };
        let history_restored = restored_task_id
            .and_then(|task_id| self.config.daemon.block_snapshot(task_id).ok())
            .is_some_and(|document| {
                document
                    .blocks
                    .iter()
                    .any(|block| block.kind != BlockKind::Task)
            });
        let session_root = self
            .config
            .state_directory
            .join("agents")
            .join(format!("session-{session_id}-{task_id}"));
        create_private_runtime_root(&session_root).map_err(|_| StartError::Driver)?;
        let mcp = match self.mcp_launch(task_id, &session_root).transpose() {
            Ok(mcp) => mcp,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(error);
            }
        };
        let launched = match self.launch_provider(provider_id, &session_root, mcp) {
            Ok(launched) => launched,
            Err(error) => {
                self.cleanup_brokered_mcp_runtime(task_id);
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(error);
            }
        };
        let (protocol, timeout) = (launched.client.protocol(), startup::timeout(provider_id));
        let thread_id = match launched.client.initialize_session(timeout) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                if agent_diagnostics_enabled() {
                    let stderr = launched.client.stderr_tail().unwrap_or_default();
                    eprintln!(
                        "hyper-term-agent: {provider_id} initialization failed: {}; stderr={}",
                        bounded_agent_diagnostic(&error.to_string()),
                        bounded_agent_diagnostic(&stderr),
                    );
                    if provider_id == "claude-acp"
                        && let Some(debug) = latest_claude_debug_tail(&session_root)
                    {
                        eprintln!(
                            "hyper-term-agent: claude-acp SDK debug tail={}",
                            bounded_agent_diagnostic(&debug),
                        );
                    }
                }
                let _ = launched.client.close();
                self.cleanup_brokered_mcp_runtime(task_id);
                let _ = std::fs::remove_dir_all(&session_root);
                return Err(StartError::Driver);
            }
        };
        let context_receipts = launched.client.execution_context_receipts();
        if !context_receipts.is_empty()
            && self
                .config
                .daemon
                .record_agent_execution_context(
                    task_id,
                    provider_id.to_owned(),
                    structured_protocol_name(protocol).into(),
                    thread_id.clone(),
                    context_receipts,
                )
                .is_err()
        {
            let _ = launched.client.close();
            self.cleanup_brokered_mcp_runtime(task_id);
            let _ = std::fs::remove_dir_all(&session_root);
            return Err(StartError::Driver);
        }
        let session = Arc::new(AgentSession {
            client: launched.client,
            provider_id: provider_id.to_owned(),
            protocol,
            task_id,
            thread_id,
            history_restored,
            runtime_root: session_root,
            progress: Mutex::new(AgentProgress {
                status: AgentStatus::Ready,
                turn_id: None,
                error: None,
            }),
            pending_effect: Mutex::new(None),
            terminals: Mutex::new(HashMap::new()),
            _managed_proxy: launched.managed_proxy,
            capability_server: launched.capability_server,
        });
        if self
            .session_bindings
            .bind(session_id, provider_id, task_id)
            .is_err()
        {
            let _ = session.client.close();
            self.cleanup_brokered_mcp_runtime(task_id);
            let _ = std::fs::remove_dir_all(&session.runtime_root);
            return Err(StartError::Driver);
        }
        let response = ready_response(session_id, &session);
        sessions.insert(session_id, session);
        Ok(response)
    }

    pub(super) fn snapshot(&self, session_id: u16) -> Result<AgentSnapshotResponse, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        let progress_status = progress.status;
        let turn_id = progress.turn_id.clone();
        let error = progress.error.clone();
        drop(progress);
        let pending_operation_id = self.pending_agent_operation(&session)?;
        let status = projected_agent_status(progress_status, pending_operation_id);
        let document = self
            .config
            .daemon
            .block_snapshot(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        let capabilities = session
            .client
            .session_capabilities()
            .map_err(|_| SessionError::Driver)?;
        let goal = session
            .client
            .thread_goal()
            .map_err(|_| SessionError::Driver)?;
        let context = self
            .config
            .daemon
            .agent_execution_context_event(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        Ok(AgentSnapshotResponse {
            session_id,
            status,
            turn_id,
            error,
            history_restored: session.history_restored,
            pending_operation_id,
            capabilities,
            goal,
            context,
            document,
        })
    }

    pub(super) fn attention(&self) -> Result<Vec<AgentAttentionSession>, SessionError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| SessionError::Lock)?
            .iter()
            .map(|(session_id, session)| (*session_id, Arc::clone(session)))
            .collect::<Vec<_>>();
        sessions.sort_by_key(|(session_id, _)| *session_id);
        sessions
            .into_iter()
            .map(|(session_id, session)| {
                let progress_status = session
                    .progress
                    .lock()
                    .map_err(|_| SessionError::Lock)?
                    .status;
                let pending_operation_id = self.pending_agent_operation(&session)?;
                let status = projected_agent_status(progress_status, pending_operation_id);
                let document_revision = self
                    .config
                    .daemon
                    .block_revision(session.task_id)
                    .map_err(|_| SessionError::Daemon)?;
                Ok(AgentAttentionSession {
                    session_id,
                    provider: session.provider_id.clone(),
                    status,
                    document_revision,
                })
            })
            .collect()
    }

    pub(super) fn stream_status(&self, session_id: u16) -> Result<AgentStatus, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        Ok(progress.status)
    }

    pub(super) fn stream_state(
        &self,
        session_id: u16,
    ) -> Result<AgentStreamStateFrame, SessionError> {
        let session = self.session(session_id)?;
        let progress = session.progress.lock().map_err(|_| SessionError::Lock)?;
        let progress_status = progress.status;
        let turn_id = progress.turn_id.clone();
        let error = progress.error.clone();
        drop(progress);
        let pending_operation_id = self.pending_agent_operation(&session)?;
        let status = projected_agent_status(progress_status, pending_operation_id);
        let document_revision = self
            .config
            .daemon
            .block_revision(session.task_id)
            .map_err(|_| SessionError::Daemon)?;
        let capabilities = session
            .client
            .session_capabilities()
            .map_err(|_| SessionError::Driver)?;
        let goal = session
            .client
            .thread_goal()
            .map_err(|_| SessionError::Driver)?;
        Ok(AgentStreamStateFrame {
            status,
            turn_id,
            error,
            history_restored: session.history_restored,
            pending_operation_id,
            document_revision,
            capabilities,
            goal,
        })
    }

    pub(super) fn pending_agent_operation(
        &self,
        session: &AgentSession,
    ) -> Result<Option<OperationId>, SessionError> {
        if let Some(operation_id) = session
            .pending_effect
            .lock()
            .map_err(|_| SessionError::Lock)?
            .as_ref()
            .map(|effect| effect.operation_id)
        {
            return Ok(Some(operation_id));
        }
        self.config
            .daemon
            .pending_operation_id(session.task_id)
            .map_err(|_| SessionError::Daemon)
    }
}
