use std::sync::{Arc, Mutex};

use hyper_term_protocol::{
    Actor, BrokeredMcpToolExecution, DomainEvent, GenUiArtifactCandidate, NewEvent,
    OperationAction, OperationCompletion, OperationId, OperationOutcome, OperationState,
    PermissionDecision, TaskId, canonical_mcp_json_bytes,
};
use sha2::{Digest, Sha256};

use crate::{
    BrokeredMcpRuntimeConfig, DaemonError, DaemonState, bounded_nonempty, is_sha256, lock,
    mcp_gateway, validate_operation_scope,
};

#[derive(Clone)]
pub(super) struct CachedBrokeredMcpExecution {
    pub(super) task_id: TaskId,
    operation_revision: u64,
    tool_name: String,
    proposal_digest: String,
    arguments_digest: String,
    execution: BrokeredMcpToolExecution,
    genui_artifact_processed: bool,
}

impl DaemonState {
    pub(crate) fn revoke_pending_brokered_mcp_operations(
        &self,
        task_id: TaskId,
        reason: &str,
    ) -> Result<usize, DaemonError> {
        let candidates = {
            let authority = lock(&self.inner.authority)?;
            authority
                .operations
                .records()
                .filter(|record| {
                    record.task_id == task_id
                        && matches!(&record.action, OperationAction::BrokeredMcpToolCall { .. })
                        && matches!(
                            record.state,
                            OperationState::WaitingHuman | OperationState::Authorized
                        )
                })
                .map(|record| record.operation_id)
                .collect::<Vec<_>>()
        };
        let mut revoked = 0;
        for operation_id in candidates {
            if matches!(
                self.cancel_brokered_mcp_operation(task_id, operation_id, reason),
                Ok(true)
            ) {
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    fn cancel_brokered_mcp_operation(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        reason: &str,
    ) -> Result<bool, DaemonError> {
        let record = self.operation(operation_id)?;
        if record.task_id != task_id
            || !matches!(record.action, OperationAction::BrokeredMcpToolCall { .. })
            || !matches!(
                record.state,
                OperationState::WaitingHuman | OperationState::Authorized
            )
        {
            return Ok(false);
        }
        if record.state == OperationState::WaitingHuman {
            self.record(NewEvent {
                task_id,
                run_id: None,
                operation_id: Some(operation_id),
                causation_id: None,
                correlation_id: None,
                payload: DomainEvent::PermissionDecided {
                    operation_revision: record.revision,
                    decision: PermissionDecision::Cancelled,
                    actor: Actor::System,
                },
            })?;
        }
        self.transition(
            task_id,
            operation_id,
            record.revision,
            OperationState::Cancelled,
            Actor::System,
            Some(reason.to_owned()),
        )?;
        Ok(true)
    }

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

    pub fn execute_brokered_mcp_tool(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        tool_name: String,
        proposal_digest: String,
        arguments: serde_json::Value,
    ) -> Result<BrokeredMcpToolExecution, DaemonError> {
        let binding = BrokeredMcpBinding::new(tool_name, proposal_digest, &arguments)?;
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        if record.state != OperationState::Dispatching {
            return Err(DaemonError::OperationNotDispatching(record.state));
        }
        binding.validate_action(&record.action)?;
        if let Some(cached) =
            self.cached_brokered_mcp_execution(task_id, operation_id, expected_revision, &binding)?
        {
            return Ok(cached.execution);
        }
        let executor = lock(&self.inner.brokered_mcp_runtimes)?
            .get(&task_id)
            .cloned()
            .ok_or(DaemonError::BrokeredMcpRuntimeMissing(task_id))?;
        let mut executor = lock(&executor)?;
        if let Some(cached) =
            self.cached_brokered_mcp_execution(task_id, operation_id, expected_revision, &binding)?
        {
            return Ok(cached.execution);
        }
        let execution = executor.execute(&binding.tool_name, &arguments);
        lock(&self.inner.brokered_mcp_executions)?.insert(
            operation_id,
            CachedBrokeredMcpExecution {
                task_id,
                operation_revision: expected_revision,
                tool_name: binding.tool_name,
                proposal_digest: binding.proposal_digest,
                arguments_digest: binding.arguments_digest,
                execution: execution.clone(),
                genui_artifact_processed: false,
            },
        );
        Ok(execution)
    }

    /// Completes the entire authorized MCP lifecycle inside the Rust authority.
    /// The Agent connector can request this operation, but cannot independently
    /// begin, execute, accept artifacts, or write completion receipts.
    pub fn run_authorized_brokered_mcp_tool(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        tool_name: String,
        proposal_digest: String,
        arguments: serde_json::Value,
    ) -> Result<BrokeredMcpToolExecution, DaemonError> {
        let binding = BrokeredMcpBinding::new(tool_name, proposal_digest, &arguments)?;
        let record = self.operation(operation_id)?;
        if record.task_id != task_id {
            return Err(DaemonError::OperationTaskMismatch {
                expected: record.task_id,
                actual: task_id,
            });
        }
        binding.validate_action(&record.action)?;
        let retry_dispatching_revision = expected_revision.checked_add(1);
        let retry_terminal_revision = expected_revision.checked_add(2);
        let dispatching_revision = match record.state {
            OperationState::Authorized => {
                validate_operation_scope(&record, task_id, expected_revision)?;
                self.begin_operation(task_id, operation_id, expected_revision)?
                    .revision
            }
            OperationState::Dispatching if retry_dispatching_revision == Some(record.revision) => {
                record.revision
            }
            OperationState::Succeeded
            | OperationState::Failed
            | OperationState::UnknownExecution
                if retry_terminal_revision == Some(record.revision) =>
            {
                return self
                    .cached_brokered_mcp_execution(
                        task_id,
                        operation_id,
                        retry_dispatching_revision
                            .expect("terminal retry has a dispatching revision"),
                        &binding,
                    )?
                    .map(|cached| cached.execution)
                    .ok_or(DaemonError::BrokeredMcpReplayMismatch);
            }
            _ if record.revision != expected_revision => {
                return Err(DaemonError::StaleOperationRevision {
                    expected: record.revision,
                    actual: expected_revision,
                });
            }
            _ => return Err(DaemonError::OperationNotAuthorized(record.state)),
        };

        let mut execution = self.execute_brokered_mcp_tool(
            task_id,
            operation_id,
            dispatching_revision,
            binding.tool_name.clone(),
            binding.proposal_digest.clone(),
            arguments,
        )?;
        let artifact_processed = self
            .cached_brokered_mcp_execution(task_id, operation_id, dispatching_revision, &binding)?
            .is_some_and(|cached| cached.genui_artifact_processed);
        if binding.tool_name == "hyper_term.genui.compile"
            && execution.outcome == OperationOutcome::Succeeded
            && !artifact_processed
        {
            execution = self.accept_brokered_genui_result(
                task_id,
                operation_id,
                dispatching_revision,
                execution,
            );
            self.update_cached_brokered_mcp_execution(operation_id, execution.clone(), true)?;
        }

        let result_digest = digest_execution(&execution)?;
        self.complete_operation(
            task_id,
            operation_id,
            dispatching_revision,
            OperationCompletion {
                executor: "hyper-term-daemon".into(),
                succeeded: execution.outcome.succeeded(),
                outcome: Some(execution.outcome),
                summary: match execution.outcome {
                    OperationOutcome::Succeeded => format!("{} completed", binding.tool_name),
                    OperationOutcome::Failed => format!("{} failed", binding.tool_name),
                    OperationOutcome::UnknownExecution => {
                        format!("{} outcome is unknown", binding.tool_name)
                    }
                },
                result_digest: Some(result_digest),
            },
        )?;
        Ok(execution)
    }

    fn accept_brokered_genui_result(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        dispatching_revision: u64,
        mut execution: BrokeredMcpToolExecution,
    ) -> BrokeredMcpToolExecution {
        let candidate = execution
            .structured_content
            .clone()
            .and_then(|value| serde_json::from_value::<GenUiArtifactCandidate>(value).ok());
        let Some(candidate) = candidate else {
            return failed_execution("GenUI compiler returned an invalid artifact candidate");
        };
        let schema_version = candidate.schema_version;
        let accepted = match self.accept_genui_artifact(
            task_id,
            operation_id,
            dispatching_revision,
            candidate,
        ) {
            Ok(accepted) => accepted,
            Err(error) => {
                return failed_execution(format!(
                    "Rust authority rejected the GenUI artifact: {error}"
                ));
            }
        };
        execution.structured_content = Some(serde_json::json!({
            "schema_version": schema_version,
            "artifact_id": accepted.artifact_id,
            "source_revision": accepted.source_revision,
            "entrypoint": accepted.entrypoint,
            "content_digest": accepted.content_digest,
            "compiler": accepted.compiler,
            "accepted_by": "rust_host",
        }));
        execution.text = format!(
            "Accepted GenUI revision {} as artifact {} ({}).",
            accepted.source_revision, accepted.artifact_id, accepted.content_digest
        );
        execution
    }

    fn cached_brokered_mcp_execution(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        operation_revision: u64,
        binding: &BrokeredMcpBinding,
    ) -> Result<Option<CachedBrokeredMcpExecution>, DaemonError> {
        let cached = lock(&self.inner.brokered_mcp_executions)?
            .get(&operation_id)
            .cloned();
        let Some(cached) = cached else {
            return Ok(None);
        };
        if cached.task_id == task_id
            && cached.operation_revision == operation_revision
            && cached.tool_name == binding.tool_name
            && cached.proposal_digest == binding.proposal_digest
            && cached.arguments_digest == binding.arguments_digest
        {
            Ok(Some(cached))
        } else {
            Err(DaemonError::BrokeredMcpReplayMismatch)
        }
    }

    fn update_cached_brokered_mcp_execution(
        &self,
        operation_id: OperationId,
        execution: BrokeredMcpToolExecution,
        genui_artifact_processed: bool,
    ) -> Result<(), DaemonError> {
        let mut executions = lock(&self.inner.brokered_mcp_executions)?;
        let cached = executions
            .get_mut(&operation_id)
            .ok_or(DaemonError::BrokeredMcpReplayMismatch)?;
        cached.execution = execution;
        cached.genui_artifact_processed = genui_artifact_processed;
        Ok(())
    }
}

struct BrokeredMcpBinding {
    tool_name: String,
    proposal_digest: String,
    arguments_digest: String,
}

impl BrokeredMcpBinding {
    fn new(
        tool_name: String,
        proposal_digest: String,
        arguments: &serde_json::Value,
    ) -> Result<Self, DaemonError> {
        let tool_name = bounded_nonempty(tool_name, 256, "brokered MCP tool name")?;
        if !is_sha256(&proposal_digest) {
            return Err(DaemonError::InvalidBrokeredMcpDigest);
        }
        let arguments_bytes = canonical_mcp_json_bytes(arguments)
            .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?;
        if arguments_bytes.len() > 1024 * 1024 {
            return Err(DaemonError::BrokeredMcpArgumentsTooLarge(
                arguments_bytes.len(),
            ));
        }
        let proposed = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });
        let recomputed_proposal_digest = sha256(
            &canonical_mcp_json_bytes(&proposed)
                .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?,
        );
        if recomputed_proposal_digest != proposal_digest {
            return Err(DaemonError::BrokeredMcpBindingMismatch);
        }
        Ok(Self {
            tool_name,
            proposal_digest,
            arguments_digest: sha256(&arguments_bytes),
        })
    }

    fn validate_action(&self, action: &OperationAction) -> Result<(), DaemonError> {
        match action {
            OperationAction::BrokeredMcpToolCall { call }
                if call.tool_name == self.tool_name
                    && call.proposal_digest == self.proposal_digest
                    && call.arguments_digest.as_str() == self.arguments_digest =>
            {
                Ok(())
            }
            _ => Err(DaemonError::BrokeredMcpBindingMismatch),
        }
    }
}

fn digest_execution(execution: &BrokeredMcpToolExecution) -> Result<String, DaemonError> {
    let value = serde_json::to_value(execution)
        .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?;
    let bytes = canonical_mcp_json_bytes(&value)
        .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?;
    Ok(sha256(&bytes))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn failed_execution(message: impl Into<String>) -> BrokeredMcpToolExecution {
    BrokeredMcpToolExecution {
        text: message.into(),
        structured_content: None,
        is_error: true,
        outcome: OperationOutcome::Failed,
    }
}
