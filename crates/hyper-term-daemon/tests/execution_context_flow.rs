#![cfg(unix)]

use std::collections::BTreeMap;

use hyper_term_daemon::{DaemonError, DaemonState};
use hyper_term_protocol::{OperationAction, OperationKind, RiskClass, TerminalCommand};
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
