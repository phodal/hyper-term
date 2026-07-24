#![cfg(unix)]

use std::collections::BTreeMap;

use hyper_term_daemon::{DaemonError, DaemonState};
use hyper_term_protocol::{
    ContextDigest, ContextReceipt, DomainEvent, EXECUTION_CONTEXT_SCHEMA_VERSION,
    EnvironmentPlanDigest, ExecutionMode, OperationAction, OperationKind, RiskClass,
    TerminalCommand,
};
use tempfile::tempdir;

#[test]
fn operation_credentials_are_rejected_before_the_durable_journal() {
    let temporary = tempdir().unwrap();
    let state_path = temporary.path().join("state");
    let state = DaemonState::open(&state_path).unwrap();
    let task_id = state
        .create_task("reject ambient credentials".into())
        .unwrap();
    let result = state.propose_operation(
        task_id,
        OperationKind::Shell,
        OperationAction::Shell {
            command: TerminalCommand {
                program: "/usr/bin/env".into(),
                args: Vec::new(),
                cwd: Some(temporary.path().to_path_buf()),
                env: BTreeMap::from([(
                    "OPENAI_API_KEY".into(),
                    "must-never-enter-the-journal".into(),
                )]),
            },
        },
        "reject a raw credential".into(),
        RiskClass::ExternalEffect,
        vec!["shell".into()],
    );
    assert!(matches!(
        result,
        Err(DaemonError::UnsafeOperationEnvironment(name)) if name == "OPENAI_API_KEY"
    ));

    let journal = std::fs::read_to_string(state_path.join("events.jsonl")).unwrap();
    assert!(!journal.contains("OPENAI_API_KEY"));
    assert!(!journal.contains("must-never-enter-the-journal"));
}

#[test]
fn agent_execution_context_receipt_is_correlated_and_survives_replay() {
    let directory = tempdir().unwrap();
    let state_path = directory.path().join("state");
    let state = DaemonState::open(&state_path).unwrap();
    let task_id = state.create_task("Codex ACP context".into()).unwrap();
    let receipt = ContextReceipt {
        schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
        context_id: "agent-provider".into(),
        context_revision: 1,
        mode: ExecutionMode::Hermetic,
        context_digest: ContextDigest::parse("a".repeat(64)).unwrap(),
        environment_digest: EnvironmentPlanDigest::parse("b".repeat(64)).unwrap(),
        clear_inherited: true,
        bindings: Vec::new(),
        credential_bindings: Vec::new(),
    };

    state
        .record_agent_execution_context(
            task_id,
            "codex-acp".into(),
            "acp".into(),
            "thread-1".into(),
            vec![receipt.clone()],
        )
        .unwrap();
    let event = state
        .agent_execution_context_event(task_id)
        .unwrap()
        .unwrap();
    assert_eq!(event.causation_id, event.correlation_id);
    assert!(event.causation_id.is_some());
    assert!(matches!(
        event.payload,
        DomainEvent::AgentExecutionContextRecorded { ref context }
            if context.provider_id == "codex-acp" && context.receipts == vec![receipt]
    ));
    drop(state);

    let reopened = DaemonState::open(&state_path).unwrap();
    let replayed = reopened
        .agent_execution_context_event(task_id)
        .unwrap()
        .unwrap();
    assert_eq!(replayed.event_id, event.event_id);
    assert_eq!(replayed.correlation_id, event.correlation_id);
    assert!(matches!(
        reopened.record_agent_execution_context(
            task_id,
            "codex-acp".into(),
            "acp".into(),
            "thread-2".into(),
            Vec::new(),
        ),
        Err(DaemonError::InvalidAgentProjection(_))
    ));
}
