#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use hyper_term_daemon::{
    AcpAgentProviderConfig, AgentGatewayConfig, AgentGenUiRuntimeConfig, DaemonState,
    spawn_agent_gateway, spawn_unix_server,
};
use hyper_term_drivers::sha256_file;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
async fn acp_agent_discovers_the_real_brokered_deno_tool_catalog() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("src/main.ts"),
        "export const answer: number = 42;\n",
    )
    .unwrap();
    let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
    let control_socket = temporary.path().join("hyperd.sock");
    let _control = spawn_unix_server(&control_socket, daemon.clone()).unwrap();

    let deno =
        PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
            .canonicalize()
            .unwrap();
    assert_eq!(
        sha256_file(&deno).unwrap(),
        std::env::var("HYPER_TERM_DENO_SHA256").expect("HYPER_TERM_DENO_SHA256")
    );
    let mcp = PathBuf::from(env!("CARGO_BIN_EXE_hyper-term-mcp"))
        .canonicalize()
        .unwrap();
    let fake_acp = temporary.path().join("fake-acp.ts");
    std::fs::write(&fake_acp, FAKE_ACP).unwrap();
    let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
    permissions.set_mode(0o400);
    std::fs::set_permissions(&fake_acp, permissions).unwrap();

    let compiler_script = temporary.path().join("genui-compiler.js");
    let compiler_wasm = temporary.path().join("esbuild.wasm");
    let preview_shell = temporary.path().join("genui-preview.html");
    std::fs::write(&compiler_script, "fixture compiler").unwrap();
    std::fs::write(&compiler_wasm, "fixture wasm").unwrap();
    std::fs::write(
        &preview_shell,
        "<!-- HYPER_TERM_ARTIFACT_BOOTSTRAP -->hyper_term_preview_boot",
    )
    .unwrap();
    let deno_cache = temporary.path().join("fake-acp-deno-cache");
    std::fs::create_dir_all(&deno_cache).unwrap();
    let token = "0123456789abcdef0123456789abcdef".to_owned();
    let gateway = spawn_agent_gateway(AgentGatewayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: token.clone(),
        workspace,
        state_directory: temporary.path().join("gateway-state"),
        daemon,
        codex_executable: None,
        codex_auth_file: None,
        acp_providers: vec![AcpAgentProviderConfig {
            provider_id: "fixture-acp".into(),
            executable: deno.clone(),
            arguments: vec![
                "run".into(),
                "--quiet".into(),
                "--no-config".into(),
                format!("--allow-run={}", mcp.display()).into(),
                fake_acp.into_os_string(),
            ],
            environment: BTreeMap::from([
                ("DENO_DIR".into(), deno_cache.into_os_string()),
                ("DENO_NO_PROMPT".into(), OsString::from("1")),
                ("DENO_NO_UPDATE_CHECK".into(), OsString::from("1")),
            ]),
            implementation_version: "fixture-1".into(),
        }],
        mcp_executable: Some(mcp),
        genui_runtime: Some(AgentGenUiRuntimeConfig {
            deno_executable: deno,
            runtime_version: "2.9.3".into(),
            compiler_script,
            compiler_wasm,
            preview_shell,
            compiler_version: "0.28.1".into(),
        }),
        workbench_assets: None,
        control_socket,
    })
    .await
    .unwrap();

    let session_path = format!("/agent/session?token={token}&session_id=7&provider=fixture-acp");
    let (status, body) = request(gateway.address(), "POST", &session_path, b"").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["protocol"], "acp-v1");

    let turn_path = format!("/agent/session/turn?token={token}&session_id=7");
    assert_eq!(
        request(
            gateway.address(),
            "POST",
            &turn_path,
            b"List your Hyper Term tools"
        )
        .await
        .0,
        202
    );
    let snapshot_path = format!("/agent/session?token={token}&session_id=7");
    let approval = loop {
        let (status, body) = request(gateway.address(), "GET", &snapshot_path, b"").await;
        assert_eq!(status, 200);
        let snapshot: Value = serde_json::from_slice(&body).unwrap();
        if let Some(approval) = snapshot["document"]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["payload"]["type"] == "approval")
        {
            break approval["payload"].clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let permission_path = format!("/agent/session/permission?token={token}&session_id=7");
    let decision = serde_json::to_vec(&serde_json::json!({
        "operation_id": approval["operation_id"],
        "expected_revision": approval["operation_revision"],
        "decision": "allow_once"
    }))
    .unwrap();
    let (status, body) = request(gateway.address(), "POST", &permission_path, &decision).await;
    assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));

    let snapshot = loop {
        let (status, body) = request(gateway.address(), "GET", &snapshot_path, b"").await;
        assert_eq!(status, 200);
        let snapshot: Value = serde_json::from_slice(&body).unwrap();
        if snapshot["status"] == "completed" {
            break snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let agent_text = snapshot["document"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["payload"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for tool in [
        "hyper_term.diff.review",
        "hyper_term.genui.compile",
        "hyper_term.lsp.query",
    ] {
        assert!(agent_text.contains(tool), "missing {tool}: {agent_text}");
    }
    assert!(
        agent_text.contains("called:textDocument/documentSymbol"),
        "LSP result did not return through ACP: {agent_text}"
    );

    gateway.shutdown().await.unwrap();
}

async fn request(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let headers = String::from_utf8_lossy(&response[..split]);
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, response[split + 4..].to_vec())
}

const FAKE_ACP: &str = r#"
const encoder = new TextEncoder();
const writer = Deno.stdout.writable.getWriter();
let tools = [];
let mcp;

async function send(message) {
  await writer.write(encoder.encode(`${JSON.stringify(message)}\n`));
}

function jsonReader(stream) {
  const reader = stream.pipeThrough(new TextDecoderStream()).getReader();
  let buffered = "";
  return async function readMessage() {
    while (true) {
      const newline = buffered.indexOf("\n");
      if (newline >= 0) {
        const line = buffered.slice(0, newline);
        buffered = buffered.slice(newline + 1);
        if (line) return JSON.parse(line);
      }
      const chunk = await reader.read();
      if (chunk.done) throw new Error("MCP output ended before its response");
      buffered += chunk.value;
    }
  };
}

async function connectMcp(server) {
  const child = new Deno.Command(server.command, {
    args: server.args,
    stdin: "piped",
    stdout: "piped",
    stderr: "null",
  }).spawn();
  const input = child.stdin.getWriter();
  const readMessage = jsonReader(child.stdout);
  async function write(message) {
    await input.write(encoder.encode(`${JSON.stringify(message)}\n`));
  }
  async function readId(id) {
    while (true) {
      const message = await readMessage();
      if (message.id === id) return message;
    }
  }
  for (const message of [
    {jsonrpc: "2.0", id: 11, method: "initialize", params: {protocolVersion: "2025-11-25", capabilities: {}, clientInfo: {name: "acp-fixture", version: "1"}}},
    {jsonrpc: "2.0", method: "notifications/initialized"},
    {jsonrpc: "2.0", id: 12, method: "tools/list", params: {}},
  ]) {
    await write(message);
  }
  await readId(11);
  const response = await readId(12);
  if (!response?.result?.tools) throw new Error("MCP tool inventory missing");
  return {
    child,
    input,
    write,
    readId,
    tools: response.result.tools.map((tool) => tool.name).sort(),
  };
}

async function queryLsp() {
  await mcp.write({
    jsonrpc: "2.0",
    id: 13,
    method: "tools/call",
    params: {
      name: "hyper_term.lsp.query",
      arguments: {
        method: "textDocument/documentSymbol",
        documentPath: "src/main.ts",
      },
    },
  });
  const response = await mcp.readId(13);
  await mcp.input.close();
  const status = await mcp.child.status;
  if (!status.success || response?.result?.isError) {
    throw new Error("MCP LSP query failed");
  }
  return response.result.structuredContent;
}

let buffered = "";
for await (const chunk of Deno.stdin.readable.pipeThrough(new TextDecoderStream())) {
  buffered += chunk;
  while (buffered.includes("\n")) {
    const newline = buffered.indexOf("\n");
    const line = buffered.slice(0, newline);
    buffered = buffered.slice(newline + 1);
    if (!line) continue;
    const request = JSON.parse(line);
    if (request.method === "initialize") {
      await send({jsonrpc: "2.0", id: request.id, result: {protocolVersion: 1, agentCapabilities: {}, authMethods: [], agentInfo: {name: "fixture-acp", version: "1"}}});
    } else if (request.method === "session/new") {
      mcp = await connectMcp(request.params.mcpServers[0]);
      tools = mcp.tools;
      await send({jsonrpc: "2.0", id: request.id, result: {sessionId: "mcp-session"}});
    } else if (request.method === "session/prompt") {
      const lsp = await queryLsp();
      const text = `${tools.join(",")}|called:${lsp.method}`;
      await send({jsonrpc: "2.0", method: "session/update", params: {sessionId: "mcp-session", update: {sessionUpdate: "agent_message_chunk", content: {type: "text", text}}}});
      await send({jsonrpc: "2.0", id: request.id, result: {stopReason: "end_turn"}});
    }
  }
}
"#;
