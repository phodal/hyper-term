#![cfg(unix)]

use std::io::{BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use hyper_term_daemon::{DaemonState, McpStdioConfig, run_mcp_stdio, spawn_unix_server};
use hyper_term_drivers::DriverFraming;
use hyper_term_protocol::{
    BlockPayload, DomainEvent, EventEnvelope, OperationState, PermissionDecision,
};
use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn approved_diff_tool_runs_through_mcp_and_leaves_a_receipt() {
    let directory = tempdir().unwrap();
    let socket = directory.path().join("hyperd.sock");
    let state_directory = directory.path().join("state");
    let state = DaemonState::open(&state_directory).unwrap();
    let _server = spawn_unix_server(&socket, state.clone()).unwrap();
    let (mut client_io, mut gateway_io) = UnixStream::pair().unwrap();
    client_io
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let gateway_input = gateway_io.try_clone().unwrap();
    let config = McpStdioConfig::new(socket.canonicalize().unwrap(), true).unwrap();
    let gateway = thread::spawn(move || run_mcp_stdio(config, gateway_input, &mut gateway_io));
    let mut output = BufReader::new(client_io.try_clone().unwrap());

    send(
        &mut client_io,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "integration-test", "version": "1"}
            }
        }),
    );
    send(
        &mut client_io,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    send(
        &mut client_io,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "hyper_term.diff.review",
                "arguments": {
                    "before": "one\ntwo\nthree\n",
                    "after": "one\nsecond\nthree\n"
                }
            }
        }),
    );
    let initialized = receive_id(&mut output, 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");

    let (task_id, operation_id, waiting_revision) = wait_for_permission(&state_directory);
    let authorized = state
        .decide_permission(
            task_id,
            operation_id,
            waiting_revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    assert_eq!(authorized.state, OperationState::Authorized);

    let result = receive_id(&mut output, 7);
    assert_eq!(result["result"]["isError"], false);
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("-two")
    );
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("+second")
    );

    let snapshot = state.block_snapshot(task_id).unwrap();
    assert!(snapshot.blocks.iter().any(|block| {
        matches!(
            block.payload,
            BlockPayload::Operation {
                operation_id: id,
                state: OperationState::Succeeded,
                ..
            } if id == operation_id
        )
    }));
    assert!(snapshot.blocks.iter().any(|block| {
        matches!(
            &block.payload,
            BlockPayload::OperationReceipt {
                operation_id: id,
                executor,
                succeeded: true,
                result_digest: Some(digest),
                ..
            } if *id == operation_id && executor == "hyper-term-mcp" && digest.len() == 64
        )
    }));

    client_io.shutdown(Shutdown::Write).unwrap();
    gateway.join().unwrap().unwrap();
}

fn send(stream: &mut UnixStream, value: Value) {
    DriverFraming::JsonLines
        .write(stream, &value, 2 * 1024 * 1024)
        .unwrap();
    stream.flush().unwrap();
}

fn receive_id(reader: &mut BufReader<UnixStream>, id: u64) -> Value {
    loop {
        let message = DriverFraming::JsonLines
            .read(reader, 2 * 1024 * 1024)
            .unwrap()
            .expect("gateway closed before response");
        if message["id"] == id {
            return message;
        }
    }
}

fn wait_for_permission(
    state_directory: &std::path::Path,
) -> (
    hyper_term_protocol::TaskId,
    hyper_term_protocol::OperationId,
    u64,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let journal =
            std::fs::read_to_string(state_directory.join("events.jsonl")).unwrap_or_default();
        for line in journal.lines() {
            let event: EventEnvelope = serde_json::from_str(line).unwrap();
            if let DomainEvent::PermissionRequested {
                operation_revision, ..
            } = event.payload
            {
                return (
                    event.task_id,
                    event.operation_id.expect("permission operation"),
                    operation_revision,
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "MCP permission proposal timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
