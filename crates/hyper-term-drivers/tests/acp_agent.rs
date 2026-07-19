use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hyper_term_drivers::{
    AcpAgentClient, AcpAgentConfig, AgentDriverEvent, AgentEffectAuthorization, DriverState,
};
use hyper_term_protocol::{OperationId, PermissionDecision};
use tempfile::TempDir;

#[test]
#[ignore = "requires HYPER_TERM_ACP_PATH, HYPER_TERM_ACP_SHA256, and an authenticated ACP adapter"]
fn installed_acp_agent_completes_a_real_prompt_without_executing_tools() {
    let executable = required_path("HYPER_TERM_ACP_PATH");
    let executable = executable
        .canonicalize()
        .expect("HYPER_TERM_ACP_PATH must resolve to the inspected adapter");
    let digest = std::env::var("HYPER_TERM_ACP_SHA256")
        .expect("HYPER_TERM_ACP_SHA256 must identify that exact adapter");
    let provider_id =
        std::env::var("HYPER_TERM_ACP_PROVIDER_ID").unwrap_or_else(|_| "test-acp".into());
    let expected = std::env::var("HYPER_TERM_ACP_EXPECTED_TEXT")
        .unwrap_or_else(|_| "HYPER_TERM_ACP_OK".into());
    let prompt = std::env::var("HYPER_TERM_ACP_PROMPT").unwrap_or_else(|_| {
        format!("Reply with exactly {expected}. Do not use tools or modify files.")
    });
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
        brokered_mcp_server: None,
    })
    .expect("launch inspected ACP adapter");

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
            AgentDriverEvent::TurnCompleted { status, .. } => break status,
            AgentDriverEvent::Exited { code, state } => {
                panic!(
                    "ACP adapter exited before completing the prompt: code={code:?} state={state:?} stderr={}",
                    client.stderr_tail().unwrap_or_default()
                );
            }
            AgentDriverEvent::Connected { .. }
            | AgentDriverEvent::PlanDelta { .. }
            | AgentDriverEvent::ThoughtDelta { .. }
            | AgentDriverEvent::ProtocolNotice { .. } => {}
        }
    };

    assert!(
        output.contains(&expected),
        "ACP answer did not contain {expected:?}: output={output:?} status={status:?} stderr={}",
        client.stderr_tail().unwrap_or_default()
    );
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

fn adapter_arguments() -> Vec<OsString> {
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
