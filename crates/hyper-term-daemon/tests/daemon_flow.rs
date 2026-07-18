#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use hyper_term_core::{TerminalEvent, TerminalReplay};
use hyper_term_daemon::{DaemonError, DaemonState, spawn_unix_server};
use hyper_term_protocol::{
    BlockPayload, ClientId, ControlRequest, ControlRequestEnvelope, ControlResponse,
    OperationAction, OperationCompletion, OperationKind, OperationState, PermissionDecision,
    RequestId, RiskClass, TerminalCommand, TerminalDataFrame, TerminalInputFrame, TerminalSize,
    WireFrame, read_frame, write_frame,
};
use tempfile::tempdir;

fn shell(script: &str) -> OperationAction {
    OperationAction::Shell {
        command: TerminalCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: BTreeMap::new(),
        },
    }
}

fn propose_shell(
    state: &DaemonState,
    task_id: hyper_term_protocol::TaskId,
    script: &str,
) -> hyper_term_core::OperationRecord {
    state
        .propose_operation(
            task_id,
            OperationKind::Shell,
            shell(script),
            "run an exact test command".into(),
            RiskClass::ReadOnly,
            vec!["shell".into()],
        )
        .expect("propose operation")
}

#[test]
fn terminal_dispatch_requires_permission_and_survives_client_subscription_drop() {
    let directory = tempdir().expect("tempdir");
    let state = DaemonState::open(directory.path()).expect("open daemon");
    let task_id = state.create_task("permission boundary".into()).unwrap();
    let proposed = propose_shell(&state, task_id, "sleep 0.05; printf durable-marker");
    assert_eq!(proposed.revision, 3);
    assert_eq!(proposed.state, OperationState::WaitingHuman);

    let error = state
        .dispatch_terminal(
            task_id,
            proposed.operation_id,
            proposed.revision,
            TerminalSize::default(),
        )
        .expect_err("unapproved operation must not execute");
    assert!(matches!(error, DaemonError::OperationNotAuthorized(_)));

    let authorized = state
        .decide_permission(
            task_id,
            proposed.operation_id,
            proposed.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    assert_eq!(authorized.revision, 4);
    assert_eq!(authorized.state, OperationState::Authorized);
    let terminal_id = state
        .dispatch_terminal(
            task_id,
            proposed.operation_id,
            authorized.revision,
            TerminalSize::default(),
        )
        .unwrap();

    drop(state.terminal_subscription(terminal_id, 0).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = state.block_snapshot(task_id).unwrap();
        let operation_succeeded = snapshot.blocks.iter().any(|block| {
            matches!(
                block.payload,
                BlockPayload::Operation {
                    operation_id,
                    state: OperationState::Succeeded,
                    ..
                } if operation_id == proposed.operation_id
            )
        });
        if operation_succeeded {
            assert!(snapshot.blocks.iter().any(|block| {
                matches!(
                    block.payload,
                    BlockPayload::Terminal {
                        terminal_id: id,
                        exit_code: Some(0),
                        ..
                    } if id == terminal_id
                )
            }));
            break;
        }
        assert!(Instant::now() < deadline, "operation did not finish");
        thread::sleep(Duration::from_millis(10));
    }

    let subscription = state.terminal_subscription(terminal_id, 0).unwrap();
    let mut bytes = match subscription.replay {
        TerminalReplay::Chunks(chunks) => chunks
            .into_iter()
            .flat_map(|chunk| chunk.bytes.iter().copied().collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        TerminalReplay::SnapshotRequired(snapshot) => snapshot.tail,
    };
    while let Ok(event) = subscription.receiver.try_recv() {
        if let TerminalEvent::Output(chunk) = event {
            bytes.extend_from_slice(&chunk.bytes);
        }
    }
    assert!(String::from_utf8_lossy(&bytes).contains("durable-marker"));

    let digest = state.block_snapshot(task_id).unwrap().semantic_digest;
    drop(state);
    let reopened = DaemonState::open(directory.path()).expect("replay journal");
    assert_eq!(
        reopened.block_snapshot(task_id).unwrap().semantic_digest,
        digest
    );
}

#[test]
fn opaque_tool_dispatch_requires_permission_and_records_its_receipt() {
    let directory = tempdir().expect("tempdir");
    let state = DaemonState::open(directory.path()).expect("open daemon");
    let task_id = state.create_task("MCP diff review".into()).unwrap();
    let proposed = state
        .propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::Opaque {
                kind: "hyper_term.diff.review".into(),
                payload_digest: "a".repeat(64),
            },
            "Build a bounded diff review".into(),
            RiskClass::ReadOnly,
            vec!["diff_review".into()],
        )
        .unwrap();
    assert_eq!(proposed.state, OperationState::WaitingHuman);
    assert!(matches!(
        state.begin_operation(task_id, proposed.operation_id, proposed.revision),
        Err(DaemonError::OperationNotAuthorized(
            OperationState::WaitingHuman
        ))
    ));

    let authorized = state
        .decide_permission(
            task_id,
            proposed.operation_id,
            proposed.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let dispatching = state
        .begin_operation(task_id, proposed.operation_id, authorized.revision)
        .unwrap();
    assert_eq!(dispatching.state, OperationState::Dispatching);
    let completed = state
        .complete_operation(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            OperationCompletion {
                executor: "hyper-term-mcp".into(),
                succeeded: true,
                summary: "Diff review produced one bounded hunk".into(),
                result_digest: Some("b".repeat(64)),
            },
        )
        .unwrap();
    assert_eq!(completed.state, OperationState::Succeeded);

    let events = std::fs::read_to_string(directory.path().join("events.jsonl")).unwrap();
    assert!(events.contains("operation_receipt"));
    assert!(events.contains("hyper-term-mcp"));
    assert!(events.contains(&"b".repeat(64)));
    assert!(
        state
            .block_snapshot(task_id)
            .unwrap()
            .blocks
            .iter()
            .any(|block| {
                matches!(
                    &block.payload,
                    BlockPayload::OperationReceipt {
                        operation_id,
                        executor,
                        succeeded: true,
                        ..
                    } if *operation_id == proposed.operation_id && executor == "hyper-term-mcp"
                )
            })
    );
}

#[test]
fn unix_client_can_reconnect_and_replay_terminal_output() {
    let directory = tempdir().expect("tempdir");
    let socket = directory.path().join("hyperd.sock");
    let state = DaemonState::open(directory.path().join("state")).unwrap();
    let _server = spawn_unix_server(&socket, state).expect("spawn server");

    let mut client = Client::connect(&socket);
    let task_id = match client.request(ControlRequest::CreateTask {
        title: "reconnect".into(),
    }) {
        ControlResponse::TaskCreated { task_id } => task_id,
        response => panic!("unexpected response: {response:?}"),
    };
    let (operation_id, waiting_revision) = match client.request(ControlRequest::ProposeOperation {
        task_id,
        kind: OperationKind::Shell,
        action: shell("sleep 0.1; printf socket-reconnect-marker"),
        summary: "reconnect proof".into(),
        risk: RiskClass::ReadOnly,
        required_capabilities: vec!["shell".into()],
    }) {
        ControlResponse::OperationUpdated {
            operation_id,
            revision,
            state: OperationState::WaitingHuman,
        } => (operation_id, revision),
        response => panic!("unexpected response: {response:?}"),
    };
    let authorized_revision = match client.request(ControlRequest::DecidePermission {
        task_id,
        operation_id,
        expected_revision: waiting_revision,
        decision: PermissionDecision::AllowOnce,
    }) {
        ControlResponse::OperationUpdated {
            revision,
            state: OperationState::Authorized,
            ..
        } => revision,
        response => panic!("unexpected response: {response:?}"),
    };
    let terminal_id = match client.request(ControlRequest::DispatchTerminal {
        task_id,
        operation_id,
        expected_revision: authorized_revision,
        size: TerminalSize::default(),
    }) {
        ControlResponse::TerminalCreated { terminal_id } => terminal_id,
        response => panic!("unexpected response: {response:?}"),
    };
    drop(client);

    thread::sleep(Duration::from_millis(150));
    let mut client = Client::connect(&socket);
    match client.request(ControlRequest::SubscribeTerminal {
        terminal_id,
        after_sequence: 0,
    }) {
        ControlResponse::TerminalSubscribed {
            terminal_id: id, ..
        } if id == terminal_id => {}
        response => panic!("unexpected response: {response:?}"),
    }
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline, "terminal replay timed out");
        match client.read() {
            WireFrame::TerminalOutput(TerminalDataFrame {
                terminal_id: id,
                bytes,
                ..
            }) if id == terminal_id => output.extend(bytes),
            WireFrame::TerminalSnapshot(snapshot) if snapshot.terminal_id == terminal_id => {
                output.extend(snapshot.bytes);
            }
            WireFrame::Response(response) => {
                if matches!(
                    response.response,
                    ControlResponse::TerminalExited {
                        terminal_id: id,
                        exit_code: Some(0),
                    } if id == terminal_id
                ) {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(String::from_utf8_lossy(&output).contains("socket-reconnect-marker"));
}

#[test]
fn unix_client_opens_the_authority_selected_user_shell() {
    let directory = tempdir().expect("tempdir");
    let socket = directory.path().join("hyperd.sock");
    let working_directory = directory.path().join("project");
    std::fs::create_dir(&working_directory).expect("create shell cwd");
    let state = DaemonState::open(directory.path().join("state")).unwrap();
    let _server = spawn_unix_server(&socket, state).expect("spawn server");

    let mut client = Client::connect(&socket);
    let terminal_id = match client.request(ControlRequest::OpenUserShell {
        cwd: Some(working_directory.clone()),
        size: TerminalSize::default(),
    }) {
        ControlResponse::TerminalCreated { terminal_id } => terminal_id,
        response => panic!("unexpected response: {response:?}"),
    };
    assert!(matches!(
        client.request(ControlRequest::ResizeTerminal {
            terminal_id,
            generation: 1,
            size: TerminalSize {
                rows: 40,
                cols: 120,
                ..TerminalSize::default()
            },
        }),
        ControlResponse::Ack
    ));
    let lease_id = match client.request(ControlRequest::AcquireInputLease {
        terminal_id,
        client_id: client.client_id,
    }) {
        ControlResponse::InputLeaseGranted { lease_id, .. } => lease_id,
        response => panic!("unexpected response: {response:?}"),
    };
    match client.request(ControlRequest::SubscribeTerminal {
        terminal_id,
        after_sequence: 0,
    }) {
        ControlResponse::TerminalSubscribed { .. } => {}
        response => panic!("unexpected response: {response:?}"),
    }
    client.send_input(TerminalInputFrame {
        terminal_id,
        lease_id,
        sequence: 1,
        bytes: b"printf '__HYPER_USER_SHELL__:%s:%s:%s\\n' \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM\"; pwd; exit\n".to_vec(),
    });

    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "user shell output timed out");
        match client.read() {
            WireFrame::TerminalOutput(frame) if frame.terminal_id == terminal_id => {
                output.extend(frame.bytes);
            }
            WireFrame::TerminalSnapshot(snapshot) if snapshot.terminal_id == terminal_id => {
                output.extend(snapshot.bytes);
            }
            _ => {}
        }
        let text = String::from_utf8_lossy(&output);
        if text.contains("__HYPER_USER_SHELL__:xterm-256color:truecolor:HyperTerm")
            && text.contains(&working_directory.display().to_string())
        {
            break;
        }
    }
}

#[test]
fn terminal_input_requires_the_active_client_lease() {
    let directory = tempdir().expect("tempdir");
    let state = DaemonState::open(directory.path()).expect("open daemon");
    let task_id = state.create_task("input lease".into()).unwrap();
    let proposed = propose_shell(&state, task_id, "cat");
    let authorized = state
        .decide_permission(
            task_id,
            proposed.operation_id,
            proposed.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let terminal_id = state
        .dispatch_terminal(
            task_id,
            proposed.operation_id,
            authorized.revision,
            TerminalSize::default(),
        )
        .unwrap();

    let owner = ClientId::new();
    let other = ClientId::new();
    let (lease_id, generation) = state.acquire_input_lease(terminal_id, owner).unwrap();
    assert_eq!(generation, 1);
    assert!(matches!(
        state.acquire_input_lease(terminal_id, other),
        Err(DaemonError::InputLeaseHeld(id)) if id == terminal_id
    ));
    assert!(matches!(
        state.write_terminal_input(
            other,
            TerminalInputFrame {
                terminal_id,
                lease_id,
                sequence: 1,
                bytes: b"not-authorized\n".to_vec(),
            }
        ),
        Err(DaemonError::InputLeaseMismatch(id)) if id == terminal_id
    ));
    state
        .write_terminal_input(
            owner,
            TerminalInputFrame {
                terminal_id,
                lease_id,
                sequence: 1,
                bytes: b"lease-marker\n".to_vec(),
            },
        )
        .unwrap();
    state
        .release_input_lease(terminal_id, lease_id, owner)
        .unwrap();
    let (_, next_generation) = state.acquire_input_lease(terminal_id, other).unwrap();
    assert_eq!(next_generation, 2);
    state.close_terminal(terminal_id).unwrap();
}

struct Client {
    stream: UnixStream,
    client_id: ClientId,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("connect: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let client_id = ClientId::new();
        let mut client = Self { stream, client_id };
        let response = client.request(ControlRequest::Hello {
            client_id,
            protocol_version: hyper_term_protocol::PROTOCOL_VERSION,
        });
        assert!(matches!(response, ControlResponse::Welcome { .. }));
        client
    }

    fn request(&mut self, request: ControlRequest) -> ControlResponse {
        let request_id = RequestId::new();
        write_frame(
            &mut self.stream,
            &WireFrame::Request(ControlRequestEnvelope {
                request_id,
                request,
            }),
        )
        .expect("write request");
        loop {
            if let WireFrame::Response(response) = self.read()
                && response.request_id == Some(request_id)
            {
                return response.response;
            }
        }
    }

    fn read(&mut self) -> WireFrame {
        read_frame(&mut self.stream).expect("read frame")
    }

    fn send_input(&mut self, frame: TerminalInputFrame) {
        write_frame(&mut self.stream, &WireFrame::TerminalInput(frame)).expect("write input");
    }
}
