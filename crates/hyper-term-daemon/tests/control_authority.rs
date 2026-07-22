#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use hyper_term_daemon::{
    ControlClient, ControlClientError, DaemonState, spawn_agent_capability_server,
};
use hyper_term_protocol::{
    ApprovalDetailDigest, BlockId, ControlRequest, ControlResponse, MessageRole, OperationAction,
    OperationKind, OperationState, RiskClass, TerminalSize, WireFrame,
};
use tempfile::tempdir;

const TIMEOUT: Duration = Duration::from_secs(3);

#[test]
fn agent_capability_endpoint_is_private_task_scoped_and_cannot_self_approve() {
    let directory = tempdir().unwrap();
    let socket_root = directory.path().join("agent-session");
    let socket = socket_root.join("mcp.sock");
    let state = DaemonState::open(directory.path().join("state")).unwrap();
    let bound_task = state.create_task("bound Agent task".into()).unwrap();
    let other_task = state.create_task("other task".into()).unwrap();
    let _server = spawn_agent_capability_server(&socket, state.clone(), bound_task).unwrap();

    assert_eq!(
        std::fs::metadata(&socket_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let mut client = ControlClient::connect(&socket, TIMEOUT).unwrap();
    assert_denied(client.request(
        ControlRequest::CreateTask {
            title: "forged task".into(),
        },
        TIMEOUT,
    ));
    assert_denied(client.request(
        ControlRequest::OpenUserShell {
            cwd: None,
            size: TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        TIMEOUT,
    ));
    assert_denied(client.request(
        ControlRequest::GetBlockSnapshot {
            task_id: bound_task,
        },
        TIMEOUT,
    ));
    assert_denied(client.request(
        ControlRequest::ProposeOperation {
            task_id: bound_task,
            kind: OperationKind::McpTool,
            action: OperationAction::Opaque {
                kind: "hyper_term.diff.review".into(),
                payload_digest: "a".repeat(64),
            },
            summary: "forged generic proposal".into(),
            risk: RiskClass::ReadOnly,
            required_capabilities: vec!["diff_review".into()],
        },
        TIMEOUT,
    ));
    assert_denied(client.request(proposal(other_task), TIMEOUT));

    let (operation_id, revision) = match client.request(proposal(bound_task), TIMEOUT).unwrap() {
        ControlResponse::OperationUpdated {
            operation_id,
            revision,
            state: OperationState::WaitingHuman,
        } => (operation_id, revision),
        response => panic!("unexpected proposal response: {response:?}"),
    };
    assert_denied(client.request(
        ControlRequest::DecidePermission {
            task_id: bound_task,
            operation_id,
            expected_revision: revision,
            approval_detail_digest: ApprovalDetailDigest::parse("a".repeat(64)).unwrap(),
            decision: hyper_term_protocol::PermissionDecision::AllowOnce,
        },
        TIMEOUT,
    ));

    while client.recv_timeout(Duration::from_millis(20)).is_ok() {}

    state
        .append_message(
            other_task,
            BlockId::new(),
            MessageRole::Agent,
            None,
            "other task event".into(),
        )
        .unwrap();
    state
        .append_message(
            bound_task,
            BlockId::new(),
            MessageRole::Agent,
            None,
            "bound task event".into(),
        )
        .unwrap();

    for _ in 0..8 {
        match client.recv_timeout(TIMEOUT).unwrap() {
            WireFrame::Response(envelope) => match envelope.response {
                ControlResponse::Event { event } if event.task_id == bound_task => return,
                ControlResponse::Event { event } => {
                    panic!(
                        "capability endpoint leaked event for task {}",
                        event.task_id
                    )
                }
                _ => {}
            },
            frame => panic!("unexpected capability frame: {frame:?}"),
        }
    }
    panic!("bound task event was not delivered");
}

fn proposal(task_id: hyper_term_protocol::TaskId) -> ControlRequest {
    ControlRequest::ProposeBrokeredMcpTool {
        task_id,
        tool_name: "hyper_term.diff.review".into(),
        arguments: serde_json::json!({"before": "a", "after": "b"}),
    }
}

fn assert_denied(response: Result<ControlResponse, ControlClientError>) {
    assert!(matches!(
        response.unwrap(),
        ControlResponse::Error { code, message }
            if code == "authority_denied"
                && message == "request is not allowed for this connection"
    ));
}
