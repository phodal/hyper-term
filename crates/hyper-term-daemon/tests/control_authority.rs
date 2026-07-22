#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use hyper_term_daemon::{
    BrokeredMcpRuntimeConfig, ControlClient, ControlClientError, DaemonState,
    spawn_agent_capability_server,
};
use hyper_term_protocol::{
    ApprovalDetailDigest, BlockId, ControlRequest, ControlResponse, GenUiArtifactCandidate,
    GenUiCompilerIdentity, MessageRole, OperationAction, OperationCompletion, OperationKind,
    OperationOutcome, OperationState, RiskClass, TerminalSize, WireFrame, canonical_mcp_json_bytes,
};
use sha2::{Digest, Sha256};
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
    state
        .register_brokered_mcp_runtime(bound_task, BrokeredMcpRuntimeConfig::default())
        .unwrap();
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
    let proposal_digest = diff_proposal_digest();
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
    assert_denied(client.request(
        ControlRequest::BeginOperation {
            task_id: bound_task,
            operation_id,
            expected_revision: revision,
        },
        TIMEOUT,
    ));
    assert_denied(client.request(
        ControlRequest::ExecuteBrokeredMcpTool {
            task_id: bound_task,
            operation_id,
            expected_revision: revision,
            tool_name: "hyper_term.diff.review".into(),
            proposal_digest: proposal_digest.clone(),
            arguments: diff_arguments(),
        },
        TIMEOUT,
    ));
    assert_denied(client.request(
        ControlRequest::CompleteOperation {
            task_id: bound_task,
            operation_id,
            expected_revision: revision,
            completion: OperationCompletion {
                executor: "forged-agent".into(),
                succeeded: true,
                outcome: Some(OperationOutcome::Succeeded),
                summary: "forged receipt".into(),
                result_digest: None,
            },
        },
        TIMEOUT,
    ));
    assert_denied(client.request(
        ControlRequest::AcceptGenUiArtifact {
            task_id: bound_task,
            operation_id,
            expected_revision: revision,
            candidate: genui_candidate(),
        },
        TIMEOUT,
    ));

    let authorized = state
        .decide_permission(
            bound_task,
            operation_id,
            revision,
            hyper_term_protocol::PermissionDecision::AllowOnce,
        )
        .unwrap();
    let first_execution = run_authorized_diff(
        &mut client,
        bound_task,
        operation_id,
        authorized.revision,
        &proposal_digest,
    );
    assert!(!first_execution.is_error, "{first_execution:?}");
    assert_eq!(first_execution.outcome, OperationOutcome::Succeeded);
    let replayed_execution = run_authorized_diff(
        &mut client,
        bound_task,
        operation_id,
        authorized.revision,
        &proposal_digest,
    );
    assert_eq!(replayed_execution, first_execution);

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
        arguments: diff_arguments(),
    }
}

fn diff_arguments() -> serde_json::Value {
    serde_json::json!({"before": "a", "after": "b"})
}

fn diff_proposal_digest() -> String {
    Sha256::digest(
        canonical_mcp_json_bytes(&serde_json::json!({
            "name": "hyper_term.diff.review",
            "arguments": diff_arguments(),
        }))
        .unwrap(),
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

fn run_authorized_diff(
    client: &mut ControlClient,
    task_id: hyper_term_protocol::TaskId,
    operation_id: hyper_term_protocol::OperationId,
    revision: u64,
    proposal_digest: &str,
) -> hyper_term_protocol::BrokeredMcpToolExecution {
    match client
        .request(
            ControlRequest::RunAuthorizedBrokeredMcpTool {
                task_id,
                operation_id,
                expected_revision: revision,
                tool_name: "hyper_term.diff.review".into(),
                proposal_digest: proposal_digest.into(),
                arguments: diff_arguments(),
            },
            TIMEOUT,
        )
        .unwrap()
    {
        ControlResponse::BrokeredMcpToolExecuted { execution } => execution,
        response => panic!("unexpected run response: {response:?}"),
    }
}

fn genui_candidate() -> GenUiArtifactCandidate {
    GenUiArtifactCandidate {
        schema_version: 1,
        source_revision: 1,
        entrypoint: "/App.tsx".into(),
        source_files: BTreeMap::from([("/App.tsx".into(), "export default () => null;".into())]),
        bundle: "bundle".into(),
        css: String::new(),
        source_map: "{}".into(),
        content_digest: "a".repeat(64),
        compiler: GenUiCompilerIdentity {
            name: "esbuild-wasm".into(),
            version: "test".into(),
        },
        diagnostics: Vec::new(),
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
