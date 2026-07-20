use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use hyper_term_drivers::{
    AcpAgentClient, AcpAgentConfig, AcpMcpServerConfig, AgentDriverEvent, AgentEffectAuthorization,
    AgentHostResponse, DriverState, sha256_file,
};
use hyper_term_protocol::{OperationId, PermissionDecision};
use tempfile::TempDir;

#[cfg(unix)]
#[test]
#[ignore = "requires HYPER_TERM_ACP_RUNTIME_ROOT, HYPER_TERM_DENO_PATH, and HYPER_TERM_DENO_SHA256"]
fn bundled_codex_acp_completes_the_release_initialize_handshake() {
    let (client, runtime) = launch_bundled_codex_acp();
    if let Err(error) = client.initialize(Duration::from_secs(20)) {
        let provider_log = fs::read_to_string(runtime.path().join("provider.log"))
            .unwrap_or_else(|log_error| format!("provider log unavailable: {log_error}"));
        panic!(
            "bundled Codex ACP initialize failed: {error}; provider={provider_log}; stderr={}",
            client.stderr_tail().unwrap_or_default()
        );
    }
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    let provider_log = fs::read_to_string(runtime.path().join("provider.log"))
        .expect("bundled adapter must launch the configured provider path");
    assert!(provider_log.contains("started: app-server"));
    assert!(provider_log.contains("\"method\":\"initialize\""));
    assert!(provider_log.contains("\"name\":\"hyper-term\""));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[cfg(unix)]
#[test]
#[ignore = "requires HYPER_TERM_ACP_RUNTIME_ROOT, HYPER_TERM_DENO_PATH, and HYPER_TERM_DENO_SHA256"]
fn bundled_claude_acp_creates_a_release_session_with_external_provider() {
    let (client, runtime) = launch_bundled_claude_acp();
    client
        .initialize(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_release_stderr(&client, &runtime, "initialize", error));
    let session_id = client
        .start_session(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_release_stderr(&client, &runtime, "session/new", error));

    assert!(!session_id.is_empty());
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    let provider_log = fs::read_to_string(runtime.path().join("provider.log"))
        .expect("bundled adapter must launch the configured provider path");
    assert!(provider_log.contains("started:"));
    assert!(provider_log.contains("\"type\":\"control_request\""));
    assert!(provider_log.contains("\"subtype\":\"initialize\""));
    assert!(provider_log.contains("\"subtype\":\"get_context_usage\""));
    assert!(provider_log.contains("--input-format stream-json"));
    assert!(provider_log.contains("--output-format stream-json"));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
#[ignore = "requires HYPER_TERM_ACP_PATH, HYPER_TERM_ACP_SHA256, and an installed ACP adapter"]
fn installed_acp_agent_completes_a_real_initialize_handshake() {
    let (client, _workspace) = launch_installed_acp_agent();
    client
        .initialize(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "initialize", error));
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
#[ignore = "requires HYPER_TERM_ACP_PATH, HYPER_TERM_ACP_SHA256, and an installed ACP adapter"]
fn installed_acp_agent_creates_a_real_session_without_prompt() {
    let (client, _workspace) = launch_installed_acp_agent_with_mcp(None);
    client
        .initialize(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "initialize", error));
    let session_id = client
        .start_session(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "session/new", error));

    assert!(!session_id.is_empty());
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
#[ignore = "requires an installed ACP adapter plus HYPER_TERM_MCP_PATH and HYPER_TERM_MCP_ARGS_JSON"]
fn installed_acp_agent_creates_a_real_session_with_brokered_mcp_without_prompt() {
    let mcp_executable = required_path("HYPER_TERM_MCP_PATH")
        .canonicalize()
        .expect("HYPER_TERM_MCP_PATH must resolve to the inspected connector");
    let arguments = serde_json::from_str::<Vec<String>>(
        &std::env::var("HYPER_TERM_MCP_ARGS_JSON").expect("HYPER_TERM_MCP_ARGS_JSON"),
    )
    .expect("HYPER_TERM_MCP_ARGS_JSON must be a JSON string array")
    .into_iter()
    .map(OsString::from)
    .collect();
    let mcp = AcpMcpServerConfig {
        executable_sha256: sha256_file(&mcp_executable).expect("digest MCP connector"),
        executable: mcp_executable,
        arguments,
    };
    let (client, _workspace) = launch_installed_acp_agent_with_mcp(Some(mcp));
    client
        .initialize(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "initialize", error));
    let session_id = client
        .start_session(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "session/new", error));

    assert!(!session_id.is_empty());
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
#[ignore = "requires HYPER_TERM_ACP_PATH, HYPER_TERM_ACP_SHA256, and an authenticated ACP adapter"]
fn installed_acp_agent_completes_a_real_prompt_without_executing_tools() {
    let (client, _workspace) = launch_installed_acp_agent();
    let expected = std::env::var("HYPER_TERM_ACP_EXPECTED_TEXT")
        .unwrap_or_else(|_| "HYPER_TERM_ACP_OK".into());
    let prompt = std::env::var("HYPER_TERM_ACP_PROMPT").unwrap_or_else(|_| {
        format!("Reply with exactly {expected}. Do not use tools or modify files.")
    });

    client
        .initialize(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "initialize", error));
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    let session_id = client
        .start_session(Duration::from_secs(20))
        .unwrap_or_else(|error| panic_with_stderr(&client, "session/new", error));
    assert!(!session_id.is_empty());
    let turn_id = client
        .start_turn(&session_id, &prompt)
        .unwrap_or_else(|error| panic_with_stderr(&client, "session/prompt", error));
    assert!(!turn_id.is_empty());

    let mut output = String::new();
    let status = loop {
        let event = client
            .next_event(Duration::from_secs(120))
            .unwrap_or_else(|error| panic_with_stderr(&client, "prompt stream", error));
        match event {
            AgentDriverEvent::MessageDelta { text, .. } => output.push_str(&text),
            AgentDriverEvent::EffectProposed { proposal, .. } => {
                client
                    .resolve_effect(
                        &proposal.request_id,
                        AgentEffectAuthorization {
                            operation_id: OperationId::new(),
                            operation_revision: 1,
                            proposal_sha256: proposal.payload_sha256,
                            decision: PermissionDecision::Cancelled,
                        },
                    )
                    .unwrap_or_else(|error| {
                        panic_with_stderr(&client, "reject unexpected tool", error)
                    });
            }
            AgentDriverEvent::HostRequest { request, .. } => {
                client
                    .resolve_host_request(
                        &request.request_id,
                        AgentHostResponse::Error {
                            code: -32601,
                            message: "Terminal requests are disabled for this integration gate"
                                .into(),
                        },
                    )
                    .unwrap_or_else(|error| {
                        panic_with_stderr(&client, "reject unexpected host request", error)
                    });
            }
            AgentDriverEvent::TurnCompleted { status, .. } => break status,
            AgentDriverEvent::Exited { code, state } => {
                panic!(
                    "ACP adapter exited before completing the prompt: code={code:?} state={state:?} stderr={}",
                    client.stderr_tail().unwrap_or_default()
                );
            }
            AgentDriverEvent::Connected { .. }
            | AgentDriverEvent::PlanDelta { .. }
            | AgentDriverEvent::PlanUpdated { .. }
            | AgentDriverEvent::ToolCallUpdated { .. }
            | AgentDriverEvent::ThoughtDelta { .. }
            | AgentDriverEvent::ProtocolNotice { .. } => {}
        }
    };

    assert!(
        output.contains(&expected),
        "ACP answer did not contain {expected:?}: output={output:?} status={status:?} stderr={}",
        client.stderr_tail().unwrap_or_default()
    );
    if let Ok(expected_command) = std::env::var("HYPER_TERM_ACP_EXPECT_COMMAND") {
        let capabilities = client
            .session_capabilities()
            .unwrap_or_else(|error| panic_with_stderr(&client, "session capabilities", error));
        assert!(
            capabilities
                .available_commands
                .iter()
                .any(|command| command.name == expected_command),
            "ACP command catalog did not retain {expected_command:?}: {:?}",
            capabilities
                .available_commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

fn launch_installed_acp_agent() -> (AcpAgentClient, TempDir) {
    launch_installed_acp_agent_with_mcp(None)
}

#[cfg(unix)]
fn launch_bundled_codex_acp() -> (AcpAgentClient, TempDir) {
    let runtime_root = bundled_acp_runtime_root();
    let entrypoint = runtime_root
        .join("node_modules/@agentclientprotocol/codex-acp/dist/index.js")
        .canonicalize()
        .expect("built Codex ACP entrypoint");
    assert!(
        entrypoint.starts_with(&runtime_root),
        "Codex ACP entrypoint escaped the built runtime"
    );
    let executable = required_path("HYPER_TERM_DENO_PATH")
        .canonicalize()
        .expect("HYPER_TERM_DENO_PATH must resolve to the inspected Deno runtime");
    let executable_sha256 = std::env::var("HYPER_TERM_DENO_SHA256")
        .expect("HYPER_TERM_DENO_SHA256 must identify that exact Deno runtime");

    let root = TempDir::new().expect("temporary bundled ACP release gate");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("temporary ACP workspace");
    let provider = root.path().join("codex");
    fs::write(
        &provider,
        "#!/bin/sh\nset -eu\nprintf 'started: %s\\n' \"$*\" > \"$HOME/provider.log\"\nIFS= read -r line\nprintf 'request: %s\\n' \"$line\" >> \"$HOME/provider.log\"\nprintf '%s\\n' '{\"id\":0,\"result\":{\"userAgent\":\"hyper-term-release-gate\"}}'\nwhile IFS= read -r line; do :; done\n",
    )
    .expect("deterministic Codex app-server fixture");
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))
        .expect("executable Codex app-server fixture");

    let arguments = bundled_deno_arguments(entrypoint);
    let mut environment = adapter_environment(&executable);
    environment.insert("HOME".into(), root.path().as_os_str().to_owned());
    environment.insert("CODEX_PATH".into(), provider.into_os_string());
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable,
        executable_sha256,
        arguments,
        environment,
        implementation_version: "bundled-release-gate".into(),
        provider_id: "codex-acp".into(),
        workspace,
        brokered_mcp_server: None,
        containment: None,
        terminal_client: false,
    })
    .expect("launch bundled Codex ACP adapter");
    (client, root)
}

#[cfg(unix)]
fn launch_bundled_claude_acp() -> (AcpAgentClient, TempDir) {
    let runtime_root = bundled_acp_runtime_root();
    let entrypoint = runtime_root
        .join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js")
        .canonicalize()
        .expect("built Claude ACP entrypoint");
    assert!(
        entrypoint.starts_with(&runtime_root),
        "Claude ACP entrypoint escaped the built runtime"
    );
    let executable = required_path("HYPER_TERM_DENO_PATH")
        .canonicalize()
        .expect("HYPER_TERM_DENO_PATH must resolve to the inspected Deno runtime");
    let executable_sha256 = std::env::var("HYPER_TERM_DENO_SHA256")
        .expect("HYPER_TERM_DENO_SHA256 must identify that exact Deno runtime");

    let root = TempDir::new().expect("temporary bundled Claude ACP release gate");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("temporary ACP workspace");
    let provider_script = root.path().join("claude-fixture.js");
    fs::write(
        &provider_script,
        r#"const logPath = `${Deno.env.get("HOME")}/provider.log`;
const appendLog = (line) => Deno.writeTextFileSync(logPath, `${line}\n`, { append: true });
appendLog(`started: ${Deno.args.join(" ")}`);

const encoder = new TextEncoder();
const send = async (message) => {
  await Deno.stdout.write(encoder.encode(`${JSON.stringify(message)}\n`));
};
const success = (requestId, response) => ({
  type: "control_response",
  response: { subtype: "success", request_id: requestId, response },
});
const decoder = new TextDecoder();
let buffer = "";
for await (const chunk of Deno.stdin.readable) {
  buffer += decoder.decode(chunk, { stream: true });
  while (buffer.includes("\n")) {
    const newline = buffer.indexOf("\n");
    const line = buffer.slice(0, newline).trim();
    buffer = buffer.slice(newline + 1);
    if (!line) continue;
    appendLog(`request: ${line}`);
    const frame = JSON.parse(line);
    if (frame.type !== "control_request") continue;
    const subtype = frame.request?.subtype;
    let response = {};
    if (subtype === "initialize") {
      response = {
        commands: [],
        agents: [],
        output_style: "default",
        available_output_styles: ["default"],
        models: [{
          value: "claude-release-gate",
          displayName: "Claude Release Gate",
          description: "Deterministic packaged-runtime fixture",
          supportsEffort: false,
          supportsAdaptiveThinking: false,
          supportsFastMode: false,
          supportsAutoMode: false,
        }],
        account: { apiProvider: "firstParty" },
      };
    } else if (subtype === "get_context_usage") {
      response = { model: "claude-release-gate", rawMaxTokens: 200000 };
    } else if (subtype === "supported_agents") {
      response = { agents: [] };
    }
    await send(success(frame.request_id, response));
  }
}
"#,
    )
    .expect("deterministic Claude stream-json fixture");
    let provider = root.path().join("claude");
    fs::write(
        &provider,
        "#!/bin/sh\nset -eu\nexec \"$HYPER_TERM_PROVIDER_DENO\" run --no-config -A \"$HYPER_TERM_PROVIDER_SCRIPT\" \"$@\"\n",
    )
    .expect("deterministic Claude executable fixture");
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))
        .expect("executable Claude fixture");

    let arguments = bundled_deno_arguments(entrypoint);
    let mut environment = adapter_environment(&executable);
    environment.insert("HOME".into(), root.path().as_os_str().to_owned());
    environment.insert("CLAUDE_CODE_EXECUTABLE".into(), provider.into_os_string());
    environment.insert(
        "HYPER_TERM_PROVIDER_DENO".into(),
        executable.as_os_str().to_owned(),
    );
    environment.insert(
        "HYPER_TERM_PROVIDER_SCRIPT".into(),
        provider_script.into_os_string(),
    );
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable,
        executable_sha256,
        arguments,
        environment,
        implementation_version: "bundled-release-gate".into(),
        provider_id: "claude-acp".into(),
        workspace,
        brokered_mcp_server: None,
        containment: None,
        terminal_client: false,
    })
    .expect("launch bundled Claude ACP adapter");
    (client, root)
}

fn bundled_acp_runtime_root() -> PathBuf {
    required_path("HYPER_TERM_ACP_RUNTIME_ROOT")
        .canonicalize()
        .expect("HYPER_TERM_ACP_RUNTIME_ROOT must resolve to the built ACP runtime")
}

fn bundled_deno_arguments(entrypoint: PathBuf) -> Vec<OsString> {
    [
        "run",
        "--cached-only",
        "--no-config",
        "--node-modules-dir=manual",
        "-A",
    ]
    .into_iter()
    .map(OsString::from)
    .chain(std::iter::once(entrypoint.into_os_string()))
    .collect()
}

fn launch_installed_acp_agent_with_mcp(
    brokered_mcp_server: Option<AcpMcpServerConfig>,
) -> (AcpAgentClient, TempDir) {
    let executable = required_path("HYPER_TERM_ACP_PATH");
    let executable = executable
        .canonicalize()
        .expect("HYPER_TERM_ACP_PATH must resolve to the inspected adapter");
    let digest = std::env::var("HYPER_TERM_ACP_SHA256")
        .expect("HYPER_TERM_ACP_SHA256 must identify that exact adapter");
    let provider_id =
        std::env::var("HYPER_TERM_ACP_PROVIDER_ID").unwrap_or_else(|_| "test-acp".into());
    let workspace = TempDir::new().expect("temporary ACP workspace");
    let arguments = adapter_arguments();
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: digest,
        arguments,
        environment: adapter_environment(&executable),
        implementation_version: "installed-e2e".into(),
        provider_id,
        workspace: workspace.path().canonicalize().unwrap(),
        brokered_mcp_server,
        containment: None,
        terminal_client: false,
    })
    .expect("launch inspected ACP adapter");
    (client, workspace)
}

fn adapter_arguments() -> Vec<OsString> {
    if let Some(arguments) = std::env::var_os("HYPER_TERM_ACP_ARGS_JSON") {
        return serde_json::from_str::<Vec<String>>(
            arguments
                .to_str()
                .expect("HYPER_TERM_ACP_ARGS_JSON must be UTF-8"),
        )
        .expect("HYPER_TERM_ACP_ARGS_JSON must be a JSON string array")
        .into_iter()
        .map(OsString::from)
        .collect();
    }
    let Some(entrypoint) = std::env::var_os("HYPER_TERM_ACP_DENO_ENTRYPOINT") else {
        return Vec::new();
    };
    let entrypoint = PathBuf::from(entrypoint)
        .canonicalize()
        .expect("HYPER_TERM_ACP_DENO_ENTRYPOINT must resolve inside the built runtime");
    [
        "run",
        "--cached-only",
        "--no-config",
        "--node-modules-dir=manual",
        "-A",
    ]
    .into_iter()
    .map(OsString::from)
    .chain(std::iter::once(entrypoint.into_os_string()))
    .collect()
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("{name} must select an inspected binary")),
    )
}

fn adapter_environment(executable: &Path) -> BTreeMap<String, OsString> {
    let home = std::env::var_os("HYPER_TERM_ACP_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .expect("HOME or HYPER_TERM_ACP_HOME must select the adapter credential home");
    let mut paths = vec![
        executable.parent().unwrap().to_owned(),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    paths.dedup();
    let path = std::env::join_paths(paths).expect("bounded ACP PATH");
    let mut environment = BTreeMap::from([
        ("HOME".into(), home),
        ("PATH".into(), path),
        ("TERM".into(), "dumb".into()),
        ("NO_BROWSER".into(), "1".into()),
        ("DENO_NO_UPDATE_CHECK".into(), "1".into()),
        ("DENO_NO_PROMPT".into(), "1".into()),
    ]);
    for name in ["USER", "LOGNAME"] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(name.into(), value);
        }
    }
    if let Some(codex) = std::env::var_os("HYPER_TERM_CODEX_PATH") {
        environment.insert("CODEX_PATH".into(), codex);
    }
    if let Some(claude) = std::env::var_os("HYPER_TERM_CLAUDE_PATH") {
        environment.insert("CLAUDE_CODE_EXECUTABLE".into(), claude);
    }
    environment
}

fn panic_with_stderr(client: &AcpAgentClient, stage: &str, error: impl std::fmt::Display) -> ! {
    panic!(
        "ACP {stage} failed: {error}; stderr={}",
        client.stderr_tail().unwrap_or_default()
    )
}

fn panic_with_release_stderr(
    client: &AcpAgentClient,
    runtime: &TempDir,
    stage: &str,
    error: impl std::fmt::Display,
) -> ! {
    let provider_log = fs::read_to_string(runtime.path().join("provider.log"))
        .unwrap_or_else(|log_error| format!("provider log unavailable: {log_error}"));
    panic!(
        "bundled ACP {stage} failed: {error}; provider={provider_log}; stderr={}",
        client.stderr_tail().unwrap_or_default()
    )
}
