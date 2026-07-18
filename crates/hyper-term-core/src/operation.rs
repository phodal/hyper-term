use std::collections::HashMap;

use hyper_term_protocol::{
    DomainEvent, EventEnvelope, OperationAction, OperationId, OperationKind, OperationState,
    PermissionDecision, RiskClass,
};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRecord {
    pub task_id: hyper_term_protocol::TaskId,
    pub operation_id: OperationId,
    pub revision: u64,
    pub kind: OperationKind,
    pub action: OperationAction,
    pub summary: String,
    pub risk: RiskClass,
    pub required_capabilities: Vec<String>,
    pub state: OperationState,
    pub permission_decision: Option<PermissionDecision>,
}

#[derive(Clone, Default)]
pub struct OperationReducer {
    records: HashMap<OperationId, OperationRecord>,
}

impl OperationReducer {
    pub fn apply(&mut self, event: &EventEnvelope) -> Result<(), OperationError> {
        match &event.payload {
            DomainEvent::OperationProposed {
                revision,
                kind,
                action,
                summary,
                risk,
                required_capabilities,
            } => {
                let operation_id = event
                    .operation_id
                    .ok_or(OperationError::MissingOperationId)?;
                if *revision != 1 {
                    return Err(OperationError::InvalidInitialRevision(*revision));
                }
                if self.records.contains_key(&operation_id) {
                    return Err(OperationError::AlreadyExists(operation_id));
                }
                self.records.insert(
                    operation_id,
                    OperationRecord {
                        task_id: event.task_id,
                        operation_id,
                        revision: *revision,
                        kind: kind.clone(),
                        action: action.clone(),
                        summary: summary.clone(),
                        risk: *risk,
                        required_capabilities: required_capabilities.clone(),
                        state: OperationState::Proposed,
                        permission_decision: None,
                    },
                );
            }
            DomainEvent::OperationStateChanged {
                revision, from, to, ..
            } => {
                let operation_id = event
                    .operation_id
                    .ok_or(OperationError::MissingOperationId)?;
                let record = self
                    .records
                    .get_mut(&operation_id)
                    .ok_or(OperationError::NotFound(operation_id))?;
                let expected_revision = record.revision + 1;
                if *revision != expected_revision {
                    return Err(OperationError::StaleRevision {
                        expected: expected_revision,
                        actual: *revision,
                    });
                }
                if record.state != *from {
                    return Err(OperationError::StateMismatch {
                        expected: record.state,
                        actual: *from,
                    });
                }
                if !valid_transition(*from, *to) {
                    return Err(OperationError::InvalidTransition {
                        from: *from,
                        to: *to,
                    });
                }
                record.revision = *revision;
                record.state = *to;
            }
            DomainEvent::PermissionRequested {
                operation_revision, ..
            } => {
                let record = self.record_for_event(event)?;
                if record.state != OperationState::WaitingHuman {
                    return Err(OperationError::PermissionOutsideWaitingState(record.state));
                }
                if record.revision != *operation_revision {
                    return Err(OperationError::StaleRevision {
                        expected: record.revision,
                        actual: *operation_revision,
                    });
                }
            }
            DomainEvent::PermissionDecided {
                operation_revision,
                decision,
                ..
            } => {
                let operation_id = event
                    .operation_id
                    .ok_or(OperationError::MissingOperationId)?;
                let record = self
                    .records
                    .get_mut(&operation_id)
                    .ok_or(OperationError::NotFound(operation_id))?;
                if record.state != OperationState::WaitingHuman {
                    return Err(OperationError::PermissionOutsideWaitingState(record.state));
                }
                if record.revision != *operation_revision {
                    return Err(OperationError::StaleRevision {
                        expected: record.revision,
                        actual: *operation_revision,
                    });
                }
                record.permission_decision = Some(*decision);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn get(&self, operation_id: OperationId) -> Option<&OperationRecord> {
        self.records.get(&operation_id)
    }

    fn record_for_event(&self, event: &EventEnvelope) -> Result<&OperationRecord, OperationError> {
        let operation_id = event
            .operation_id
            .ok_or(OperationError::MissingOperationId)?;
        self.records
            .get(&operation_id)
            .ok_or(OperationError::NotFound(operation_id))
    }
}

pub fn valid_transition(from: OperationState, to: OperationState) -> bool {
    use OperationState as S;
    matches!(
        (from, to),
        (S::Proposed, S::PolicyCheck)
            | (S::PolicyCheck, S::WaitingHuman)
            | (S::PolicyCheck, S::Authorized)
            | (S::PolicyCheck, S::Failed)
            | (S::PolicyCheck, S::Cancelled)
            | (S::WaitingHuman, S::Authorized)
            | (S::WaitingHuman, S::Cancelled)
            | (S::Authorized, S::Dispatching)
            | (S::Authorized, S::Cancelled)
            | (S::Dispatching, S::Succeeded)
            | (S::Dispatching, S::Failed)
            | (S::Dispatching, S::Cancelled)
            | (S::Dispatching, S::UnknownExecution)
            | (S::UnknownExecution, S::Succeeded)
            | (S::UnknownExecution, S::Failed)
            | (S::UnknownExecution, S::Cancelled)
    )
}

#[derive(Debug, Error)]
pub enum OperationError {
    #[error("operation event is missing operation_id")]
    MissingOperationId,
    #[error("operation {0} already exists")]
    AlreadyExists(OperationId),
    #[error("operation {0} does not exist")]
    NotFound(OperationId),
    #[error("operation initial revision must be 1, got {0}")]
    InvalidInitialRevision(u64),
    #[error("stale operation revision: expected {expected}, got {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("operation state mismatch: expected {expected:?}, got {actual:?}")]
    StateMismatch {
        expected: OperationState,
        actual: OperationState,
    },
    #[error("invalid operation transition {from:?} -> {to:?}")]
    InvalidTransition {
        from: OperationState,
        to: OperationState,
    },
    #[error("permission event is invalid while operation is {0:?}")]
    PermissionOutsideWaitingState(OperationState),
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::{Actor, EVENT_SCHEMA_VERSION, EventId, TaskId, TerminalCommand};

    use super::*;

    fn envelope(sequence: u64, operation_id: OperationId, payload: DomainEvent) -> EventEnvelope {
        EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            sequence,
            event_id: EventId::new(),
            recorded_at_ms: sequence,
            task_id: TaskId::new(),
            run_id: None,
            operation_id: Some(operation_id),
            causation_id: None,
            correlation_id: None,
            payload,
        }
    }

    #[test]
    fn operation_requires_explicit_revisioned_transitions() {
        let operation_id = OperationId::new();
        let mut reducer = OperationReducer::default();
        reducer
            .apply(&envelope(
                1,
                operation_id,
                DomainEvent::OperationProposed {
                    revision: 1,
                    kind: OperationKind::Shell,
                    action: OperationAction::Shell {
                        command: TerminalCommand {
                            program: "cargo".into(),
                            args: vec!["test".into()],
                            cwd: None,
                            env: Default::default(),
                        },
                    },
                    summary: "run tests".into(),
                    risk: RiskClass::ReadOnly,
                    required_capabilities: vec!["shell".into()],
                },
            ))
            .unwrap();
        reducer
            .apply(&envelope(
                2,
                operation_id,
                DomainEvent::OperationStateChanged {
                    revision: 2,
                    from: OperationState::Proposed,
                    to: OperationState::PolicyCheck,
                    actor: Actor::Policy,
                    reason: None,
                },
            ))
            .unwrap();
        assert_eq!(
            reducer.get(operation_id).unwrap().state,
            OperationState::PolicyCheck
        );
    }

    #[test]
    fn operation_rejects_skipping_permission_and_dispatch_states() {
        let operation_id = OperationId::new();
        let mut reducer = OperationReducer::default();
        reducer
            .apply(&envelope(
                1,
                operation_id,
                DomainEvent::OperationProposed {
                    revision: 1,
                    kind: OperationKind::Shell,
                    action: OperationAction::Shell {
                        command: TerminalCommand {
                            program: "rm".into(),
                            args: vec!["target".into()],
                            cwd: None,
                            env: Default::default(),
                        },
                    },
                    summary: "delete".into(),
                    risk: RiskClass::Destructive,
                    required_capabilities: vec![],
                },
            ))
            .unwrap();
        let error = reducer
            .apply(&envelope(
                2,
                operation_id,
                DomainEvent::OperationStateChanged {
                    revision: 2,
                    from: OperationState::Proposed,
                    to: OperationState::Succeeded,
                    actor: Actor::Agent {
                        adapter: "test".into(),
                    },
                    reason: None,
                },
            ))
            .expect_err("must reject transition");
        assert!(matches!(error, OperationError::InvalidTransition { .. }));
    }
}
