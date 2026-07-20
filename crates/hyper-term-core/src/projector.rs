use std::collections::BTreeMap;

use hyper_term_protocol::{
    AttentionState, BLOCK_SCHEMA_VERSION, BlockAction, BlockDocument, BlockEnvelope, BlockId,
    BlockKind, BlockLifecycle, BlockOperation, BlockPatch, BlockPayload, DomainEvent,
    EventEnvelope, OperationOutcome, OperationState, PermissionDecision, RenderSlot, RiskClass,
    TaskId, TrustClass,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone)]
pub struct BlockProjector {
    task_id: TaskId,
    revision: u64,
    blocks: BTreeMap<BlockId, BlockEnvelope>,
}

impl BlockProjector {
    pub fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            revision: 0,
            blocks: BTreeMap::new(),
        }
    }

    pub fn replay<'a>(
        task_id: TaskId,
        events: impl IntoIterator<Item = &'a EventEnvelope>,
    ) -> Result<Self, ProjectorError> {
        let mut projector = Self::new(task_id);
        for event in events {
            if event.task_id == task_id {
                projector.apply(event)?;
            }
        }
        Ok(projector)
    }

    pub fn apply(&mut self, event: &EventEnvelope) -> Result<BlockPatch, ProjectorError> {
        if event.task_id != self.task_id {
            return Err(ProjectorError::WrongTask {
                expected: self.task_id,
                actual: event.task_id,
            });
        }
        if event.sequence <= self.revision {
            return Err(ProjectorError::StaleEvent {
                current: self.revision,
                actual: event.sequence,
            });
        }
        let base_revision = self.revision;
        let operations = match &event.payload {
            DomainEvent::TaskCreated { title } => {
                let block_id = stable_block_id("task", self.task_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Task,
                    event.sequence,
                    BlockPayload::Task {
                        title: title.clone(),
                    },
                );
                block.render_slot = RenderSlot::SessionHeader;
                block.trust_class = TrustClass::TrustedChrome;
                block.lifecycle = BlockLifecycle::Running;
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::MessageAppended {
                block_id,
                role,
                text,
                ..
            } => {
                if let Some(block) = self.blocks.get_mut(block_id) {
                    let previous_revision = block.block_revision;
                    match &mut block.payload {
                        BlockPayload::Message {
                            role: existing_role,
                            text: existing_text,
                        } if existing_role == role => {
                            existing_text.push_str(text);
                            block.block_revision += 1;
                            block.document_revision = event.sequence;
                        }
                        _ => return Err(ProjectorError::BlockKindMismatch(*block_id)),
                    }
                    vec![BlockOperation::AppendContent {
                        block_id: *block_id,
                        expected_previous_revision: previous_revision,
                        block_revision: previous_revision + 1,
                        text: text.clone(),
                    }]
                } else {
                    let block = BlockEnvelope::new(
                        *block_id,
                        self.task_id,
                        BlockKind::Message,
                        event.sequence,
                        BlockPayload::Message {
                            role: *role,
                            text: text.clone(),
                        },
                    );
                    vec![self.upsert(block, event.sequence)?]
                }
            }
            DomainEvent::OperationProposed {
                kind,
                action: _,
                summary,
                risk,
                required_capabilities,
                ..
            } => {
                let operation_id = event
                    .operation_id
                    .ok_or(ProjectorError::MissingOperationId)?;
                let block_id = stable_block_id("operation", operation_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Operation,
                    event.sequence,
                    BlockPayload::Operation {
                        operation_id,
                        kind: kind.clone(),
                        summary: summary.clone(),
                        risk: *risk,
                        required_capabilities: required_capabilities.clone(),
                        state: OperationState::Proposed,
                    },
                );
                block.trust_class = TrustClass::TrustedChrome;
                block.lifecycle = BlockLifecycle::Queued;
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::OperationStateChanged { to, .. } => {
                let operation_id = event
                    .operation_id
                    .ok_or(ProjectorError::MissingOperationId)?;
                let block_id = stable_block_id("operation", operation_id.to_string());
                let mut block = self
                    .blocks
                    .get(&block_id)
                    .cloned()
                    .ok_or(ProjectorError::MissingBlock(block_id))?;
                match &mut block.payload {
                    BlockPayload::Operation { state, .. } => *state = *to,
                    _ => return Err(ProjectorError::BlockKindMismatch(block_id)),
                }
                block.lifecycle = lifecycle_for_operation(*to);
                block.attention = attention_for_operation(*to);
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::PermissionRequested {
                operation_revision,
                prompt,
                options,
            } => {
                let operation_id = event
                    .operation_id
                    .ok_or(ProjectorError::MissingOperationId)?;
                let block_id = stable_block_id("approval", operation_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Approval,
                    event.sequence,
                    BlockPayload::Approval {
                        operation_id,
                        operation_revision: *operation_revision,
                        prompt: prompt.clone(),
                        options: options.clone(),
                        decision: None,
                    },
                );
                block.render_slot = RenderSlot::Attention;
                block.trust_class = TrustClass::TrustedChrome;
                block.lifecycle = BlockLifecycle::Waiting;
                block.attention = AttentionState::WaitingApproval;
                block.actions = options
                    .iter()
                    .map(|option| BlockAction {
                        action_id: permission_action_id(*option).into(),
                        expected_block_revision: block.block_revision,
                        risk: RiskClass::ExternalEffect,
                        required_capabilities: Vec::new(),
                    })
                    .collect();
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::PermissionDecided { decision, .. } => {
                let operation_id = event
                    .operation_id
                    .ok_or(ProjectorError::MissingOperationId)?;
                let block_id = stable_block_id("approval", operation_id.to_string());
                let mut block = self
                    .blocks
                    .get(&block_id)
                    .cloned()
                    .ok_or(ProjectorError::MissingBlock(block_id))?;
                match &mut block.payload {
                    BlockPayload::Approval {
                        decision: existing, ..
                    } => *existing = Some(*decision),
                    _ => return Err(ProjectorError::BlockKindMismatch(block_id)),
                }
                block.lifecycle = match decision {
                    PermissionDecision::AllowOnce | PermissionDecision::AllowAlways => {
                        BlockLifecycle::Succeeded
                    }
                    PermissionDecision::RejectOnce
                    | PermissionDecision::RejectAlways
                    | PermissionDecision::Cancelled => BlockLifecycle::Cancelled,
                };
                block.attention = AttentionState::None;
                block.actions.clear();
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::OperationReceipt {
                operation_revision,
                executor,
                succeeded,
                outcome,
                summary,
                result_digest,
            } => {
                let outcome = outcome.unwrap_or(if *succeeded {
                    OperationOutcome::Succeeded
                } else {
                    OperationOutcome::Failed
                });
                let operation_id = event
                    .operation_id
                    .ok_or(ProjectorError::MissingOperationId)?;
                let block_id = stable_block_id("receipt", operation_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Receipt,
                    event.sequence,
                    BlockPayload::OperationReceipt {
                        operation_id,
                        operation_revision: *operation_revision,
                        executor: executor.clone(),
                        succeeded: outcome.succeeded(),
                        outcome: Some(outcome),
                        summary: summary.clone(),
                        result_digest: result_digest.clone(),
                    },
                );
                block.trust_class = TrustClass::TrustedChrome;
                block.lifecycle = match outcome {
                    OperationOutcome::Succeeded => BlockLifecycle::Succeeded,
                    OperationOutcome::Failed => BlockLifecycle::Failed,
                    OperationOutcome::UnknownExecution => BlockLifecycle::UnknownExecution,
                };
                block.attention = match outcome {
                    OperationOutcome::Succeeded => AttentionState::None,
                    OperationOutcome::Failed | OperationOutcome::UnknownExecution => {
                        AttentionState::Failed
                    }
                };
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::ArtifactAccepted { artifact } => {
                let block_id = stable_block_id("artifact", self.task_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Artifact,
                    event.sequence,
                    BlockPayload::Artifact {
                        artifact: artifact.clone(),
                    },
                );
                block.render_slot = RenderSlot::Inspector;
                block.trust_class = TrustClass::IsolatedArtifact;
                block.lifecycle = BlockLifecycle::Succeeded;
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::SandboxProfileCompiled { .. }
            | DomainEvent::SandboxLeaseIssued { .. }
            | DomainEvent::SandboxReceiptRecorded { .. }
            | DomainEvent::SandboxViolationObserved { .. }
            | DomainEvent::AgentExecutionContextRecorded { .. } => Vec::new(),
            DomainEvent::TerminalOpened {
                terminal_id,
                command,
                size,
            } => {
                let block_id = stable_block_id("terminal", terminal_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Terminal,
                    event.sequence,
                    BlockPayload::Terminal {
                        terminal_id: *terminal_id,
                        command: command.display_text(),
                        size: size.clone(),
                        resize_generation: 0,
                        stream_sequence: 0,
                        byte_count: 0,
                        exit_code: None,
                    },
                );
                block.trust_class = TrustClass::TrustedChrome;
                block.lifecycle = BlockLifecycle::Running;
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::TerminalOutputObserved {
                terminal_id,
                stream_sequence,
                byte_count,
            } => {
                let block_id = stable_block_id("terminal", terminal_id.to_string());
                let mut block = self
                    .blocks
                    .get(&block_id)
                    .cloned()
                    .ok_or(ProjectorError::MissingBlock(block_id))?;
                match &mut block.payload {
                    BlockPayload::Terminal {
                        stream_sequence: existing_sequence,
                        byte_count: existing_bytes,
                        ..
                    } => {
                        if *stream_sequence <= *existing_sequence {
                            return Err(ProjectorError::StaleTerminalSequence {
                                current: *existing_sequence,
                                actual: *stream_sequence,
                            });
                        }
                        *existing_sequence = *stream_sequence;
                        *existing_bytes = existing_bytes.saturating_add(*byte_count);
                    }
                    _ => return Err(ProjectorError::BlockKindMismatch(block_id)),
                }
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::TerminalResized {
                terminal_id,
                generation,
                size,
            } => {
                let block_id = stable_block_id("terminal", terminal_id.to_string());
                let mut block = self
                    .blocks
                    .get(&block_id)
                    .cloned()
                    .ok_or(ProjectorError::MissingBlock(block_id))?;
                match &mut block.payload {
                    BlockPayload::Terminal {
                        resize_generation,
                        size: existing_size,
                        ..
                    } => {
                        if *generation <= *resize_generation {
                            return Err(ProjectorError::StaleResizeGeneration {
                                current: *resize_generation,
                                actual: *generation,
                            });
                        }
                        *resize_generation = *generation;
                        *existing_size = size.clone();
                    }
                    _ => return Err(ProjectorError::BlockKindMismatch(block_id)),
                }
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::TerminalExited {
                terminal_id,
                exit_code,
            } => {
                let block_id = stable_block_id("terminal", terminal_id.to_string());
                let mut block = self
                    .blocks
                    .get(&block_id)
                    .cloned()
                    .ok_or(ProjectorError::MissingBlock(block_id))?;
                match &mut block.payload {
                    BlockPayload::Terminal {
                        exit_code: existing_exit,
                        ..
                    } => *existing_exit = *exit_code,
                    _ => return Err(ProjectorError::BlockKindMismatch(block_id)),
                }
                block.lifecycle = if exit_code == &Some(0) {
                    BlockLifecycle::Succeeded
                } else {
                    BlockLifecycle::Failed
                };
                block.attention = if exit_code == &Some(0) {
                    AttentionState::None
                } else {
                    AttentionState::Failed
                };
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::ReviewReady { summary } => {
                let block_id = stable_block_id("review", self.task_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Review,
                    event.sequence,
                    BlockPayload::Review {
                        summary: summary.clone(),
                    },
                );
                block.render_slot = RenderSlot::Attention;
                block.trust_class = TrustClass::TrustedChrome;
                block.lifecycle = BlockLifecycle::Succeeded;
                block.attention = AttentionState::ReviewReady;
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::Diagnostic { code, message } => {
                let block_id = stable_block_id("diagnostic", event.event_id.to_string());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::Diagnostic,
                    event.sequence,
                    BlockPayload::Diagnostic {
                        code: code.clone(),
                        message: message.clone(),
                    },
                );
                block.lifecycle = BlockLifecycle::Failed;
                block.attention = AttentionState::Failed;
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::AgentToolCallUpdated { turn_id, call } => {
                let block_id = stable_block_id(
                    "agent-tool-call",
                    format!("{turn_id}:{}", call.tool_call_id),
                );
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::AgentToolCall,
                    event.sequence,
                    BlockPayload::AgentToolCall {
                        turn_id: turn_id.clone(),
                        call: call.clone(),
                    },
                );
                block.lifecycle = match call.status {
                    hyper_term_protocol::AgentToolStatus::Pending => BlockLifecycle::Queued,
                    hyper_term_protocol::AgentToolStatus::InProgress => BlockLifecycle::Running,
                    hyper_term_protocol::AgentToolStatus::Completed => BlockLifecycle::Succeeded,
                    hyper_term_protocol::AgentToolStatus::Failed => BlockLifecycle::Failed,
                };
                block.attention = if call.status == hyper_term_protocol::AgentToolStatus::Failed {
                    AttentionState::Failed
                } else {
                    AttentionState::None
                };
                vec![self.upsert(block, event.sequence)?]
            }
            DomainEvent::AgentPlanUpdated { turn_id, entries } => {
                let block_id = stable_block_id("agent-plan", turn_id.clone());
                let mut block = BlockEnvelope::new(
                    block_id,
                    self.task_id,
                    BlockKind::AgentPlan,
                    event.sequence,
                    BlockPayload::AgentPlan {
                        turn_id: turn_id.clone(),
                        entries: entries.clone(),
                    },
                );
                block.lifecycle = if entries
                    .iter()
                    .all(|entry| entry.status == hyper_term_protocol::AgentPlanStatus::Completed)
                {
                    BlockLifecycle::Succeeded
                } else {
                    BlockLifecycle::Running
                };
                vec![self.upsert(block, event.sequence)?]
            }
        };

        self.revision = event.sequence;
        Ok(BlockPatch {
            stream_sequence: event.sequence,
            base_revision,
            target_revision: event.sequence,
            operations,
        })
    }

    pub fn snapshot(&self) -> Result<BlockDocument, ProjectorError> {
        let mut blocks = self.blocks.values().cloned().collect::<Vec<_>>();
        blocks.sort_by_key(|block| (block.order_key, block.block_id));
        let semantic_digest = document_digest(self.task_id, self.revision, &blocks)?;
        Ok(BlockDocument {
            schema_version: BLOCK_SCHEMA_VERSION,
            task_id: self.task_id,
            revision: self.revision,
            semantic_digest,
            blocks,
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn upsert(
        &mut self,
        mut block: BlockEnvelope,
        document_revision: u64,
    ) -> Result<BlockOperation, ProjectorError> {
        if let Some(existing) = self.blocks.get(&block.block_id) {
            if existing.kind != block.kind {
                return Err(ProjectorError::BlockKindMismatch(block.block_id));
            }
            block.block_revision = existing.block_revision + 1;
            block.order_key = existing.order_key;
        }
        block.document_revision = document_revision;
        self.blocks.insert(block.block_id, block.clone());
        Ok(BlockOperation::Upsert { block })
    }
}

pub fn apply_patch(document: &mut BlockDocument, patch: &BlockPatch) -> Result<(), ProjectorError> {
    if document.revision != patch.base_revision {
        return Err(ProjectorError::PatchBaseMismatch {
            expected: document.revision,
            actual: patch.base_revision,
        });
    }
    for operation in &patch.operations {
        match operation {
            BlockOperation::Upsert { block } => {
                if let Some(existing) = document
                    .blocks
                    .iter_mut()
                    .find(|existing| existing.block_id == block.block_id)
                {
                    if existing.kind != block.kind {
                        return Err(ProjectorError::BlockKindMismatch(block.block_id));
                    }
                    *existing = block.clone();
                } else {
                    document.blocks.push(block.clone());
                }
            }
            BlockOperation::AppendContent {
                block_id,
                expected_previous_revision,
                block_revision,
                text,
            } => {
                let block = document
                    .blocks
                    .iter_mut()
                    .find(|block| block.block_id == *block_id)
                    .ok_or(ProjectorError::MissingBlock(*block_id))?;
                if block.block_revision != *expected_previous_revision {
                    return Err(ProjectorError::BlockRevisionMismatch {
                        expected: block.block_revision,
                        actual: *expected_previous_revision,
                    });
                }
                match &mut block.payload {
                    BlockPayload::Message { text: existing, .. } => existing.push_str(text),
                    _ => return Err(ProjectorError::BlockKindMismatch(*block_id)),
                }
                block.block_revision = *block_revision;
                block.document_revision = patch.target_revision;
            }
            BlockOperation::Remove { block_id } => {
                document.blocks.retain(|block| block.block_id != *block_id);
            }
        }
    }
    document
        .blocks
        .sort_by_key(|block| (block.order_key, block.block_id));
    document.revision = patch.target_revision;
    document.semantic_digest =
        document_digest(document.task_id, document.revision, &document.blocks)?;
    Ok(())
}

fn lifecycle_for_operation(state: OperationState) -> BlockLifecycle {
    match state {
        OperationState::Proposed | OperationState::PolicyCheck => BlockLifecycle::Queued,
        OperationState::WaitingHuman => BlockLifecycle::Waiting,
        OperationState::Authorized | OperationState::Dispatching => BlockLifecycle::Running,
        OperationState::Succeeded => BlockLifecycle::Succeeded,
        OperationState::Failed | OperationState::Violated => BlockLifecycle::Failed,
        OperationState::Cancelled => BlockLifecycle::Cancelled,
        OperationState::UnknownExecution => BlockLifecycle::UnknownExecution,
    }
}

fn attention_for_operation(state: OperationState) -> AttentionState {
    match state {
        OperationState::WaitingHuman => AttentionState::WaitingApproval,
        OperationState::Failed | OperationState::Violated | OperationState::UnknownExecution => {
            AttentionState::Failed
        }
        _ => AttentionState::None,
    }
}

fn permission_action_id(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::AllowOnce => "allow_once",
        PermissionDecision::AllowAlways => "allow_always",
        PermissionDecision::RejectOnce => "reject_once",
        PermissionDecision::RejectAlways => "reject_always",
        PermissionDecision::Cancelled => "cancel",
    }
}

fn stable_block_id(scope: &str, identity: String) -> BlockId {
    BlockId::from_uuid(Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("hyper-term:{scope}:{identity}").as_bytes(),
    ))
}

#[derive(Serialize)]
struct DigestInput<'a> {
    schema_version: u16,
    task_id: TaskId,
    revision: u64,
    blocks: &'a [BlockEnvelope],
}

fn document_digest(
    task_id: TaskId,
    revision: u64,
    blocks: &[BlockEnvelope],
) -> Result<String, ProjectorError> {
    let bytes = serde_json::to_vec(&DigestInput {
        schema_version: BLOCK_SCHEMA_VERSION,
        task_id,
        revision,
        blocks,
    })?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Error)]
pub enum ProjectorError {
    #[error("event belongs to task {actual}, expected {expected}")]
    WrongTask { expected: TaskId, actual: TaskId },
    #[error("stale event sequence {actual}; projector is at {current}")]
    StaleEvent { current: u64, actual: u64 },
    #[error("operation event is missing operation_id")]
    MissingOperationId,
    #[error("block {0} does not exist")]
    MissingBlock(BlockId),
    #[error("block {0} changed kind or payload schema")]
    BlockKindMismatch(BlockId),
    #[error("stale terminal stream sequence {actual}; current is {current}")]
    StaleTerminalSequence { current: u64, actual: u64 },
    #[error("stale resize generation {actual}; current is {current}")]
    StaleResizeGeneration { current: u64, actual: u64 },
    #[error("patch base {actual} does not match document revision {expected}")]
    PatchBaseMismatch { expected: u64, actual: u64 },
    #[error("block revision mismatch: document is {expected}, patch expects {actual}")]
    BlockRevisionMismatch { expected: u64, actual: u64 },
    #[error("failed to serialize semantic digest: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::{
        AcceptedGenUiArtifact, AgentPlanEntry, AgentPlanPriority, AgentPlanStatus, AgentToolCall,
        AgentToolContent, AgentToolKind, AgentToolLocation, AgentToolStatus, ArtifactId,
        EVENT_SCHEMA_VERSION, EventEnvelope, EventId, GenUiCompilerIdentity, MessageRole,
        OperationId, OperationKind, RiskClass,
    };

    use super::*;

    fn event(
        sequence: u64,
        task_id: TaskId,
        operation_id: Option<OperationId>,
        payload: DomainEvent,
    ) -> EventEnvelope {
        EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            sequence,
            event_id: EventId::new(),
            recorded_at_ms: sequence,
            task_id,
            run_id: None,
            operation_id,
            causation_id: None,
            correlation_id: None,
            payload,
        }
    }

    #[test]
    fn snapshot_plus_patches_matches_full_replay() {
        let task_id = TaskId::new();
        let message_id = BlockId::new();
        let events = [
            event(
                1,
                task_id,
                None,
                DomainEvent::TaskCreated {
                    title: "build terminal".into(),
                },
            ),
            event(
                2,
                task_id,
                None,
                DomainEvent::MessageAppended {
                    block_id: message_id,
                    role: MessageRole::Agent,
                    external_message_id: Some("agent-1".into()),
                    text: "hello ".into(),
                },
            ),
            event(
                3,
                task_id,
                None,
                DomainEvent::MessageAppended {
                    block_id: message_id,
                    role: MessageRole::Agent,
                    external_message_id: Some("agent-1".into()),
                    text: "world".into(),
                },
            ),
        ];

        let mut projector = BlockProjector::new(task_id);
        projector.apply(&events[0]).unwrap();
        let mut client = projector.snapshot().unwrap();
        for event in &events[1..] {
            let patch = projector.apply(event).unwrap();
            apply_patch(&mut client, &patch).unwrap();
        }

        assert_eq!(client, projector.snapshot().unwrap());
        assert!(client.blocks.iter().any(|block| matches!(
            &block.payload,
            BlockPayload::Message { text, .. } if text == "hello world"
        )));
    }

    #[test]
    fn replay_digest_is_deterministic() {
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let events = [
            event(
                1,
                task_id,
                None,
                DomainEvent::TaskCreated {
                    title: "deterministic".into(),
                },
            ),
            event(
                2,
                task_id,
                Some(operation_id),
                DomainEvent::OperationProposed {
                    revision: 1,
                    kind: OperationKind::Shell,
                    action: hyper_term_protocol::OperationAction::Shell {
                        command: hyper_term_protocol::TerminalCommand {
                            program: "cargo".into(),
                            args: vec!["test".into()],
                            cwd: None,
                            env: Default::default(),
                        },
                    },
                    summary: "cargo test".into(),
                    risk: RiskClass::ReadOnly,
                    required_capabilities: vec!["shell".into()],
                },
            ),
        ];
        let first = BlockProjector::replay(task_id, &events)
            .unwrap()
            .snapshot()
            .unwrap();
        let second = BlockProjector::replay(task_id, &events)
            .unwrap()
            .snapshot()
            .unwrap();
        assert_eq!(first.semantic_digest, second.semantic_digest);
    }

    #[test]
    fn operation_capabilities_and_permission_options_survive_projection() {
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let events = [
            event(
                1,
                task_id,
                Some(operation_id),
                DomainEvent::OperationProposed {
                    revision: 1,
                    kind: OperationKind::Shell,
                    action: hyper_term_protocol::OperationAction::Shell {
                        command: hyper_term_protocol::TerminalCommand {
                            program: "cargo".into(),
                            args: vec!["test".into()],
                            cwd: None,
                            env: Default::default(),
                        },
                    },
                    summary: "Agent terminal in Tier 2: cargo test".into(),
                    risk: RiskClass::ExternalEffect,
                    required_capabilities: vec!["shell".into(), "sandbox.isolated_task".into()],
                },
            ),
            event(
                2,
                task_id,
                Some(operation_id),
                DomainEvent::PermissionRequested {
                    operation_revision: 3,
                    prompt: "Allow this exact operation once?".into(),
                    options: vec![
                        PermissionDecision::AllowOnce,
                        PermissionDecision::RejectOnce,
                        PermissionDecision::Cancelled,
                    ],
                },
            ),
        ];

        let snapshot = BlockProjector::replay(task_id, &events)
            .unwrap()
            .snapshot()
            .unwrap();
        assert!(snapshot.blocks.iter().any(|block| matches!(
            &block.payload,
            BlockPayload::Operation {
                operation_id: id,
                required_capabilities,
                ..
            } if *id == operation_id && required_capabilities == &["shell", "sandbox.isolated_task"]
        )));
        assert!(snapshot.blocks.iter().any(|block| matches!(
            &block.payload,
            BlockPayload::Approval {
                operation_id: id,
                options,
                ..
            } if *id == operation_id && options.contains(&PermissionDecision::AllowOnce)
        )));
    }

    #[test]
    fn a_new_artifact_replaces_the_task_last_known_good_block() {
        let task_id = TaskId::new();
        let first_id = ArtifactId::new();
        let second_id = ArtifactId::new();
        let artifact = |artifact_id, source_revision, digest: &str| AcceptedGenUiArtifact {
            artifact_id,
            source_revision,
            entrypoint: "/App.tsx".into(),
            content_digest: digest.into(),
            compiler: GenUiCompilerIdentity {
                name: "esbuild-wasm".into(),
                version: "0.28.1".into(),
            },
        };
        let events = [
            event(
                1,
                task_id,
                None,
                DomainEvent::TaskCreated {
                    title: "GenUI task".into(),
                },
            ),
            event(
                2,
                task_id,
                Some(OperationId::new()),
                DomainEvent::ArtifactAccepted {
                    artifact: artifact(first_id, 1, &"a".repeat(64)),
                },
            ),
            event(
                3,
                task_id,
                Some(OperationId::new()),
                DomainEvent::ArtifactAccepted {
                    artifact: artifact(second_id, 2, &"b".repeat(64)),
                },
            ),
        ];
        let snapshot = BlockProjector::replay(task_id, &events)
            .unwrap()
            .snapshot()
            .unwrap();
        let artifacts = snapshot
            .blocks
            .iter()
            .filter_map(|block| match &block.payload {
                BlockPayload::Artifact { artifact } => Some((block, artifact)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].1.artifact_id, second_id);
        assert_eq!(artifacts[0].1.source_revision, 2);
        assert_eq!(artifacts[0].0.trust_class, TrustClass::IsolatedArtifact);
        assert_eq!(artifacts[0].0.render_slot, RenderSlot::Inspector);
        assert_eq!(artifacts[0].0.block_revision, 2);
    }

    #[test]
    fn agent_plan_and_tool_updates_keep_stable_blocks_and_lifecycle() {
        let task_id = TaskId::new();
        let tool_call = |status| AgentToolCall {
            tool_call_id: "read-cargo".into(),
            title: "Read Cargo.toml".into(),
            kind: AgentToolKind::Read,
            status,
            content: vec![AgentToolContent::Text {
                text: "workspace members".into(),
            }],
            locations: vec![AgentToolLocation {
                path: "Cargo.toml".into(),
                line: Some(1),
            }],
            raw_input: None,
            raw_output: None,
        };
        let events = [
            event(
                1,
                task_id,
                None,
                DomainEvent::TaskCreated {
                    title: "ACP projection".into(),
                },
            ),
            event(
                2,
                task_id,
                None,
                DomainEvent::AgentPlanUpdated {
                    turn_id: "turn-1".into(),
                    entries: vec![AgentPlanEntry {
                        content: "Inspect the workspace".into(),
                        priority: AgentPlanPriority::High,
                        status: AgentPlanStatus::InProgress,
                    }],
                },
            ),
            event(
                3,
                task_id,
                None,
                DomainEvent::AgentToolCallUpdated {
                    turn_id: "turn-1".into(),
                    call: tool_call(AgentToolStatus::InProgress),
                },
            ),
            event(
                4,
                task_id,
                None,
                DomainEvent::AgentToolCallUpdated {
                    turn_id: "turn-1".into(),
                    call: tool_call(AgentToolStatus::Completed),
                },
            ),
        ];

        let snapshot = BlockProjector::replay(task_id, &events)
            .unwrap()
            .snapshot()
            .unwrap();
        let tools = snapshot
            .blocks
            .iter()
            .filter(|block| block.kind == BlockKind::AgentToolCall)
            .collect::<Vec<_>>();
        let plans = snapshot
            .blocks
            .iter()
            .filter(|block| block.kind == BlockKind::AgentPlan)
            .collect::<Vec<_>>();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].block_revision, 2);
        assert_eq!(tools[0].lifecycle, BlockLifecycle::Succeeded);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].lifecycle, BlockLifecycle::Running);
    }
}
