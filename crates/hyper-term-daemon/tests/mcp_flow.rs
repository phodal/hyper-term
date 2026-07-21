#![cfg(unix)]

use std::io::{BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use hyper_term_daemon::{
    BrokeredMcpRuntimeConfig, DaemonState, DenoGenUiMcpExecutorConfig, DenoMcpExecutorConfig,
    McpStdioConfig, run_mcp_stdio, spawn_unix_server,
};
use hyper_term_drivers::{DriverFraming, sha256_file};
use hyper_term_protocol::{
    BlockPayload, DomainEvent, EventEnvelope, OperationOutcome, OperationState, PermissionDecision,
};
use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn approved_diff_tool_runs_through_mcp_and_leaves_a_receipt() {
    let directory = tempdir().unwrap();
    let socket = directory.path().join("hyperd.sock");
    let state_directory = directory.path().join("state");
    let state = DaemonState::open(&state_directory).unwrap();
    let agent_task_id = state.create_task("Codex Agent session 1".into()).unwrap();
    let _server = spawn_unix_server(&socket, state.clone()).unwrap();
    let (mut client_io, mut gateway_io) = UnixStream::pair().unwrap();
    client_io
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let gateway_input = gateway_io.try_clone().unwrap();
    let config = McpStdioConfig::new(socket.canonicalize().unwrap(), true)
        .unwrap()
        .with_task(agent_task_id);
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
    assert_eq!(task_id, agent_task_id);
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
    assert_eq!(result["result"]["isError"], false, "{result}");
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
                outcome: Some(OperationOutcome::Succeeded),
                result_digest: Some(digest),
                ..
            } if *id == operation_id && executor == "hyper-term-mcp" && digest.len() == 64
        )
    }));

    client_io.shutdown(Shutdown::Write).unwrap();
    gateway.join().unwrap().unwrap();
}

#[test]
#[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
fn approved_lsp_tool_queries_the_pinned_deno_snapshot() {
    let directory = tempdir().unwrap();
    let socket = directory.path().join("hyperd.sock");
    let state_directory = directory.path().join("state");
    let snapshot = directory.path().join("snapshot");
    let cache = directory.path().join("deno-cache");
    let scratch = directory.path().join("deno-scratch");
    std::fs::create_dir(&snapshot).unwrap();
    std::fs::create_dir(&cache).unwrap();
    std::fs::create_dir(&scratch).unwrap();
    std::fs::write(
        snapshot.join("main.ts"),
        "export function answer(): number { return 42; }\n",
    )
    .unwrap();
    let state = DaemonState::open(&state_directory).unwrap();
    let agent_task_id = state.create_task("Codex Agent LSP".into()).unwrap();
    let deno = std::path::PathBuf::from(
        std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"),
    )
    .canonicalize()
    .unwrap();
    state
        .register_brokered_mcp_runtime(
            agent_task_id,
            BrokeredMcpRuntimeConfig {
                deno_lsp: Some(DenoMcpExecutorConfig {
                    executable: deno,
                    executable_sha256: std::env::var("HYPER_TERM_DENO_SHA256")
                        .expect("HYPER_TERM_DENO_SHA256"),
                    runtime_version: "2.9.3".into(),
                    workspace_snapshot: snapshot.canonicalize().unwrap(),
                    cache_directory: cache.canonicalize().unwrap(),
                    scratch_directory: scratch.canonicalize().unwrap(),
                }),
                deno_genui: None,
            },
        )
        .unwrap();
    let _server = spawn_unix_server(&socket, state.clone()).unwrap();
    let (mut client_io, mut gateway_io) = UnixStream::pair().unwrap();
    client_io
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let gateway_input = gateway_io.try_clone().unwrap();
    let config = McpStdioConfig::new(socket.canonicalize().unwrap(), true)
        .unwrap()
        .with_task(agent_task_id)
        .with_deno_lsp_enabled();
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
                "clientInfo": {"name": "lsp-test", "version": "1"}
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
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "hyper_term.lsp.query",
                "arguments": {
                    "method": "textDocument/documentSymbol",
                    "documentPath": "main.ts"
                }
            }
        }),
    );
    let initialized = receive_id(&mut output, 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    let (task_id, operation_id, waiting_revision) = wait_for_permission(&state_directory);
    state
        .decide_permission(
            task_id,
            operation_id,
            waiting_revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();

    let result = receive_id(&mut output, 8);
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert_eq!(
        result["result"]["structuredContent"]["method"],
        "textDocument/documentSymbol"
    );
    assert!(
        result["result"]["structuredContent"]["result"]
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())
    );
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
                        operation_id: id,
                        outcome: Some(OperationOutcome::Succeeded),
                        ..
                    } if *id == operation_id
                )
            })
    );

    client_io.shutdown(Shutdown::Write).unwrap();
    gateway.join().unwrap().unwrap();
}

#[test]
#[ignore = "requires HYPER_TERM_DENO_PATH, HYPER_TERM_DENO_SHA256, and built GenUI runtime assets"]
fn approved_genui_tool_compiles_through_the_brokered_deno_runtime() {
    let directory = tempdir().unwrap();
    let socket = directory.path().join("hyperd.sock");
    let state_directory = directory.path().join("state");
    let cache = directory.path().join("deno-cache");
    let scratch = directory.path().join("deno-scratch");
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .unwrap()
        .canonicalize()
        .unwrap();
    let compiler_script = root.join("dist/runtime/genui-compiler.js");
    let compiler_wasm = root.join("dist/runtime/esbuild.wasm");
    let state = DaemonState::open(&state_directory).unwrap();
    let agent_task_id = state.create_task("Codex Agent GenUI".into()).unwrap();
    let deno = std::path::PathBuf::from(
        std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"),
    )
    .canonicalize()
    .unwrap();
    state
        .register_brokered_mcp_runtime(
            agent_task_id,
            BrokeredMcpRuntimeConfig {
                deno_lsp: None,
                deno_genui: Some(DenoGenUiMcpExecutorConfig {
                    executable: deno,
                    executable_sha256: std::env::var("HYPER_TERM_DENO_SHA256")
                        .expect("HYPER_TERM_DENO_SHA256"),
                    runtime_version: "2.9.3".into(),
                    compiler_script_sha256: sha256_file(&compiler_script).unwrap(),
                    compiler_script,
                    compiler_wasm_sha256: sha256_file(&compiler_wasm).unwrap(),
                    compiler_wasm,
                    compiler_version: "0.28.1".into(),
                    cache_directory: cache,
                    scratch_directory: scratch,
                }),
            },
        )
        .unwrap();
    let _server = spawn_unix_server(&socket, state.clone()).unwrap();
    let (mut client_io, mut gateway_io) = UnixStream::pair().unwrap();
    client_io
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let gateway_input = gateway_io.try_clone().unwrap();
    let config = McpStdioConfig::new(socket.canonicalize().unwrap(), true)
        .unwrap()
        .with_task(agent_task_id)
        .with_deno_genui_enabled();
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
                "clientInfo": {"name": "genui-test", "version": "1"}
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
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "hyper_term.genui.compile",
                "arguments": {
                    "source": "export default function App(){ return <main data-brokered=\"true\">Ready</main>; }",
                    "entry": "App.tsx"
                }
            }
        }),
    );
    let initialized = receive_id(&mut output, 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    let (task_id, operation_id, waiting_revision) = wait_for_permission(&state_directory);
    assert_eq!(task_id, agent_task_id);
    state
        .decide_permission(
            task_id,
            operation_id,
            waiting_revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();

    let result = receive_id(&mut output, 9);
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert_eq!(
        result["result"]["structuredContent"]["compiler"]["version"],
        "0.28.1"
    );
    assert_eq!(
        result["result"]["structuredContent"]["content_digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        result["result"]["structuredContent"]["accepted_by"],
        "rust_host"
    );
    assert_eq!(
        result["result"]["structuredContent"]["artifact_id"]
            .as_str()
            .unwrap()
            .len(),
        36
    );
    assert!(
        result["result"]["structuredContent"]["bundle"]
            .as_str()
            .unwrap()
            .contains("data-brokered")
    );
    assert_eq!(
        result["result"]["structuredContent"]["source_files"]["/App.tsx"],
        "export default function App(){ return <main data-brokered=\"true\">Ready</main>; }"
    );
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
                        operation_id: id,
                        executor,
                        outcome: Some(OperationOutcome::Succeeded),
                        ..
                    } if *id == operation_id && executor == "hyper-term-mcp"
                )
            })
    );
    assert!(
        state
            .block_snapshot(task_id)
            .unwrap()
            .blocks
            .iter()
            .any(|block| matches!(
                &block.payload,
                BlockPayload::Artifact { artifact }
                    if artifact.source_revision == 1
                        && artifact.content_digest
                            == result["result"]["structuredContent"]["content_digest"]
                                .as_str()
                                .unwrap()
            ))
    );

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
