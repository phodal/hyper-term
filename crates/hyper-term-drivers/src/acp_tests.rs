use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hyper_term_protocol::OperationId;
use tempfile::TempDir;

use super::*;
use crate::acp_capabilities::MAX_AVAILABLE_COMMANDS;
use crate::{AgentCredentialBinding, AgentSessionConfigKind};

fn fake_agent(script: &str) -> (TempDir, PathBuf) {
    let temporary = TempDir::new().unwrap();
    let executable = temporary.path().join("fake-acp");
    std::fs::write(&executable, script).unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).unwrap();
    (temporary, executable)
}

fn launch(executable: &Path, workspace: &Path) -> AcpAgentClient {
    AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.to_owned(),
        executable_sha256: sha256_file(executable).unwrap(),
        arguments: vec![],
        environment: BTreeMap::new(),
        implementation_version: "fixture-1".into(),
        provider_id: "fixture-acp".into(),
        workspace: workspace.to_owned(),
        brokered_mcp_server: None,
        containment: None,
        terminal_client: false,
    })
    .unwrap()
}

#[test]
fn copilot_brokered_mcp_uses_trusted_launch_configuration() {
    let (_temporary, executable) = fake_agent("#!/bin/sh\nexit 0\n");
    let runtime = TempDir::new().unwrap();
    let config = AcpMcpServerConfig {
        executable_sha256: sha256_file(&executable).unwrap(),
        executable: executable.canonicalize().unwrap(),
        arguments: vec!["--agent-mode".into(), "--task-id".into(), "task-1".into()],
        runtime_home: runtime.path().join("home"),
        runtime_temp: runtime.path().join("tmp"),
    };
    let environment = copilot_mcp_environment(
        &config,
        &BTreeMap::from([("PATH".into(), OsString::from("/usr/bin:/bin"))]),
    )
    .unwrap();
    let arguments = copilot_mcp_arguments(&config, &environment).unwrap();

    assert_eq!(arguments[0], "--additional-mcp-config");
    let payload: Value = serde_json::from_str(arguments[1].to_str().unwrap()).unwrap();
    let server = &payload["mcpServers"]["hyper_term"];
    assert_eq!(server["type"], "local");
    assert_eq!(server["command"], config.executable.to_str().unwrap());
    assert_eq!(server["args"][0], "--agent-mode");
    assert_eq!(
        server["env"]["HOME"],
        runtime.path().join("home").to_str().unwrap()
    );
    assert_eq!(server["tools"][0], "*");
}

#[test]
fn acp_v1_streams_message_and_completion_with_official_schema() {
    let (_temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-1\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"ACP is live.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let client = launch(&executable, workspace.path());
    let initialized = client.initialize(Duration::from_secs(10)).unwrap();
    assert_eq!(initialized.protocol_version, ProtocolVersion::V1);
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    assert_eq!(session_id, "session-1");
    let turn_id = client.start_turn(&session_id, "say hello").unwrap();
    assert_eq!(turn_id, "acp-turn-3");
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::MessageDelta { text, .. } if text == "ACP is live."
    ));
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { status: Some(status), .. } if status == "end_turn"
    ));
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_cancel_notifies_agent_and_cancels_pending_permission() {
    let (_temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-cancel\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"permission-cancel\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-cancel\",\"toolCall\":{\"toolCallId\":\"tool-cancel\",\"kind\":\"execute\",\"title\":\"Run tests\"},\"options\":[{\"optionId\":\"allow-once\",\"name\":\"Allow\",\"kind\":\"allow_once\"}]}}' ;;\n    *'\"id\":\"permission-cancel\"'*'\"outcome\":\"cancelled\"'*) ;;\n    *'\"method\":\"session/cancel\"'*'\"sessionId\":\"session-cancel\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"cancelled\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let client = launch(&executable, workspace.path());
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client.start_turn(&session_id, "run tests").unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::EffectProposed { .. }
    ));

    client.cancel_turn(&session_id).unwrap();
    assert!(client.pending_effects().unwrap().is_empty());
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { status: Some(status), .. } if status == "cancelled"
    ));
    assert!(matches!(
        client.cancel_turn(&session_id),
        Err(AcpAdapterError::NoActivePrompt)
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_terminal_requests_cross_the_bounded_host_transport() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*'\"terminal\":true'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-terminal\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-create-1\",\"method\":\"terminal/create\",\"params\":{\"sessionId\":\"session-terminal\",\"command\":\"cargo\",\"args\":[\"test\",\"--workspace\"],\"env\":[{\"name\":\"RUST_LOG\",\"value\":\"warn\"}],\"outputByteLimit\":8192}}' ;;\n    *'\"id\":\"terminal-create-1\"'*'\"terminalId\":\"ht-terminal-1\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-output-1\",\"method\":\"terminal/output\",\"params\":{\"sessionId\":\"session-terminal\",\"terminalId\":\"ht-terminal-1\"}}' ;;\n    *'\"id\":\"terminal-output-1\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-wait-1\",\"method\":\"terminal/wait_for_exit\",\"params\":{\"sessionId\":\"session-terminal\",\"terminalId\":\"ht-terminal-1\"}}' ;;\n    *'\"id\":\"terminal-wait-1\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-kill-1\",\"method\":\"terminal/kill\",\"params\":{\"sessionId\":\"session-terminal\",\"terminalId\":\"ht-terminal-1\"}}' ;;\n    *'\"id\":\"terminal-kill-1\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-release-1\",\"method\":\"terminal/release\",\"params\":{\"sessionId\":\"session-terminal\",\"terminalId\":\"ht-terminal-1\"}}' ;;\n    *'\"id\":\"terminal-release-1\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: sha256_file(&executable).unwrap(),
        arguments: vec![],
        environment: BTreeMap::new(),
        implementation_version: "fixture-1".into(),
        provider_id: "fixture-acp".into(),
        workspace: workspace.clone(),
        brokered_mcp_server: None,
        containment: None,
        terminal_client: true,
    })
    .unwrap();
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client.start_turn(&session_id, "run the tests").unwrap();

    let create = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::HostRequest { request, .. } => request,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(create.method, "terminal/create");
    assert!(matches!(
        &create.operation,
        AgentHostOperation::TerminalCreate {
            command,
            args,
            env,
            cwd,
            output_byte_limit: 8192,
        } if command == "cargo"
            && args == &["test", "--workspace"]
            && env == &[AgentTerminalEnvironmentVariable {
                name: "RUST_LOG".into(),
                value: "warn".into(),
            }]
            && cwd == &workspace.canonicalize().unwrap()
    ));
    client
        .resolve_host_request(
            &create.request_id,
            AgentHostResponse::TerminalCreated {
                terminal_id: "ht-terminal-1".into(),
            },
        )
        .unwrap();

    let output = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::HostRequest { request, .. } => request,
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        output.operation,
        AgentHostOperation::TerminalOutput { ref terminal_id }
            if terminal_id == "ht-terminal-1"
    ));
    client
        .resolve_host_request(
            &output.request_id,
            AgentHostResponse::TerminalOutput {
                output: "done\n".into(),
                truncated: false,
                exit_code: Some(0),
                signal: None,
            },
        )
        .unwrap();

    let wait = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::HostRequest { request, .. } => request,
        event => panic!("unexpected event: {event:?}"),
    };
    client
        .resolve_host_request(
            &wait.request_id,
            AgentHostResponse::TerminalExited {
                exit_code: Some(0),
                signal: None,
            },
        )
        .unwrap();
    let kill = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::HostRequest { request, .. } => request,
        event => panic!("unexpected event: {event:?}"),
    };
    client
        .resolve_host_request(&kill.request_id, AgentHostResponse::TerminalKilled)
        .unwrap();
    let release = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::HostRequest { request, .. } => request,
        event => panic!("unexpected event: {event:?}"),
    };
    client
        .resolve_host_request(&release.request_id, AgentHostResponse::TerminalReleased)
        .unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_session_capabilities_are_bounded_replaced_and_configurable() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-config\",\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"gpt-a\",\"options\":[{\"value\":\"gpt-a\",\"name\":\"GPT A\"},{\"value\":\"gpt-b\",\"name\":\"GPT B\"}]}]}}' ;;\n    *'\"method\":\"session/set_config_option\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"gpt-b\",\"options\":[{\"value\":\"gpt-a\",\"name\":\"GPT A\"},{\"value\":\"gpt-b\",\"name\":\"GPT B\"}]}]}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-config\",\"update\":{\"sessionUpdate\":\"available_commands_update\",\"availableCommands\":[{\"name\":\"skills\",\"description\":\"Configure skills\"}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-config\",\"update\":{\"sessionUpdate\":\"config_option_update\",\"configOptions\":[{\"id\":\"thought\",\"name\":\"Reasoning\",\"category\":\"thought_level\",\"type\":\"select\",\"currentValue\":\"high\",\"options\":[{\"value\":\"high\",\"name\":\"High\"}]}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let client = launch(&executable, temporary.path());
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();

    let initial = client.session_capabilities().unwrap();
    assert_eq!(initial.config_options[0].id, "model");
    assert_eq!(initial.config_options[0].choices.len(), 2);
    assert!(
        client
            .set_session_config_option(
                &session_id,
                "model",
                AgentSessionConfigValue::Id {
                    value: "missing".into(),
                },
                Duration::from_secs(10),
            )
            .is_err()
    );
    let updated = client
        .set_session_config_option(
            &session_id,
            "model",
            AgentSessionConfigValue::Id {
                value: "gpt-b".into(),
            },
            Duration::from_secs(10),
        )
        .unwrap();
    assert!(matches!(
        &updated.config_options[0].kind,
        AgentSessionConfigKind::Select { current_value } if current_value == "gpt-b"
    ));

    client.start_turn(&session_id, "show capabilities").unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ProtocolNotice { .. }
    ));
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ProtocolNotice { .. }
    ));
    let capabilities = client.session_capabilities().unwrap();
    assert_eq!(capabilities.available_commands[0].name, "skills");
    assert_eq!(capabilities.config_options[0].id, "thought");
}

#[test]
fn acp_session_modes_round_trip_through_the_native_config_projection() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-modes\",\"modes\":{\"currentModeId\":\"ask\",\"availableModes\":[{\"id\":\"ask\",\"name\":\"Ask\",\"description\":\"Answer without editing\"},{\"id\":\"code\",\"name\":\"Code\",\"description\":\"Edit the workspace\"}]},\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"type\":\"select\",\"currentValue\":\"fast\",\"options\":[{\"value\":\"fast\",\"name\":\"Fast\"}]}]}}' ;;\n    *'\"method\":\"session/set_mode\"'*'\"modeId\":\"code\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-modes\",\"update\":{\"sessionUpdate\":\"current_mode_update\",\"currentModeId\":\"ask\"}}}' ;;\n  esac\ndone\n",
    );
    let client = launch(&executable, temporary.path());
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();

    let initial = client.session_capabilities().unwrap();
    assert_eq!(initial.config_options[0].id, ACP_SESSION_MODE_CONFIG_ID);
    assert!(matches!(
        &initial.config_options[0].kind,
        AgentSessionConfigKind::Select { current_value } if current_value == "ask"
    ));

    let selected = client
        .set_session_config_option(
            &session_id,
            ACP_SESSION_MODE_CONFIG_ID,
            AgentSessionConfigValue::Id {
                value: "code".into(),
            },
            Duration::from_secs(10),
        )
        .unwrap();
    assert!(matches!(
        &selected.config_options[0].kind,
        AgentSessionConfigKind::Select { current_value } if current_value == "code"
    ));
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ProtocolNotice { .. }
    ));
    let autonomously_updated = client.session_capabilities().unwrap();
    assert!(matches!(
        &autonomously_updated.config_options[0].kind,
        AgentSessionConfigKind::Select { current_value } if current_value == "ask"
    ));
    assert_eq!(autonomously_updated.config_options[1].id, "model");
}

#[test]
fn oversized_available_command_catalog_is_truncated_without_hiding_skills() {
    let mut commands = (0..MAX_AVAILABLE_COMMANDS + 12)
        .map(|index| {
            json!({
                "name": format!("command-{index}"),
                "description": format!("Command {index}"),
            })
        })
        .collect::<Vec<_>>();
    commands.push(json!({
        "name": "$review",
        "description": "Review with a skill",
    }));
    commands.push(json!({
        "name": "skills",
        "description": "Configure skills",
    }));
    commands.push(json!({
        "name": "skills",
        "description": "Duplicate skills command",
    }));
    commands.push(json!({
        "name": "x".repeat(MAX_CAPABILITY_ID_BYTES + 1),
        "description": "Invalid oversized command",
    }));
    let commands = serde_json::from_value::<Vec<v1::AvailableCommand>>(Value::Array(commands))
        .expect("ACP available commands");

    let normalized = normalize_available_commands(commands);

    assert!(normalized.truncated);
    assert_eq!(normalized.commands.len(), MAX_AVAILABLE_COMMANDS);
    assert_eq!(normalized.commands[0].name, "skills");
    assert_eq!(normalized.commands[1].name, "$review");
    assert_eq!(
        normalized
            .commands
            .iter()
            .filter(|command| command.name == "skills")
            .count(),
        1
    );
}

#[test]
fn oversized_available_command_update_does_not_abort_the_turn() {
    let mut commands = (0..MAX_AVAILABLE_COMMANDS + 10)
        .map(|index| {
            json!({
                "name": format!("command-{index}"),
                "description": format!("Command {index}"),
            })
        })
        .collect::<Vec<_>>();
    commands.push(json!({
        "name": "skills",
        "description": "Configure skills",
    }));
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "authMethods": [],
        },
    });
    let session = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": { "sessionId": "session-command-overflow" },
    });
    let update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "session-command-overflow",
            "update": {
                "sessionUpdate": "available_commands_update",
                "availableCommands": commands,
            },
        },
    });
    let completed = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "result": { "stopReason": "end_turn" },
    });
    let script = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
*'"method":"initialize"'*) printf '%s\n' '{initialize}' ;;
*'"method":"session/new"'*) printf '%s\n' '{session}' ;;
*'"method":"session/prompt"'*)
  printf '%s\n' '{update}'
  printf '%s\n' '{completed}' ;;
  esac
done
"#,
    );
    let (temporary, executable) = fake_agent(&script);
    let client = launch(&executable, temporary.path());
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client.start_turn(&session_id, "show skills").unwrap();

    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ProtocolNotice {
            method: Some(method),
            ..
        } if method == "session/update/available_commands_truncated"
    ));
    let capabilities = client.session_capabilities().unwrap();
    assert_eq!(
        capabilities.available_commands.len(),
        MAX_AVAILABLE_COMMANDS
    );
    assert_eq!(capabilities.available_commands[0].name, "skills");
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_v1_preserves_plan_tool_diff_terminal_resource_and_updates() {
    let (_temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-structured\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-structured\",\"update\":{\"sessionUpdate\":\"plan\",\"entries\":[{\"content\":\"Inspect the workspace\",\"priority\":\"high\",\"status\":\"in_progress\"}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-structured\",\"update\":{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"edit-1\",\"title\":\"Edit src/lib.rs\",\"kind\":\"edit\",\"status\":\"in_progress\",\"locations\":[{\"path\":\"/tmp/src/lib.rs\",\"line\":7}],\"content\":[{\"type\":\"diff\",\"path\":\"/tmp/src/lib.rs\",\"oldText\":\"old\\n\",\"newText\":\"new\\n\"},{\"type\":\"terminal\",\"terminalId\":\"terminal-7\"},{\"type\":\"content\",\"content\":{\"type\":\"text\",\"text\":\"Applied edit\"}},{\"type\":\"content\",\"content\":{\"type\":\"resource_link\",\"name\":\"build log\",\"uri\":\"file:///tmp/build.log\",\"mimeType\":\"text/plain\"}}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-structured\",\"update\":{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"edit-1\",\"status\":\"completed\",\"rawOutput\":{\"ok\":true}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let client = launch(&executable, workspace.path());
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client.start_turn(&session_id, "make the edit").unwrap();

    let plan = client.next_event(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        plan,
        AgentDriverEvent::PlanUpdated { entries, .. }
            if entries.len() == 1
                && entries[0].content == "Inspect the workspace"
                && entries[0].status == AgentPlanStatus::InProgress
    ));
    let tool = client.next_event(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        tool,
        AgentDriverEvent::ToolCallUpdated { call, .. }
            if call.status == AgentToolStatus::InProgress
                && call.locations.len() == 1
                && matches!(&call.content[0], AgentToolContent::Diff { added_lines: 1, removed_lines: 1, patch, .. } if patch.contains("-old") && patch.contains("+new"))
                && matches!(&call.content[1], AgentToolContent::Terminal { terminal_id } if terminal_id == "terminal-7")
                && matches!(&call.content[2], AgentToolContent::Text { text } if text == "Applied edit")
                && matches!(&call.content[3], AgentToolContent::Resource { uri, .. } if uri == "file:///tmp/build.log")
    ));
    let completed = client.next_event(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        completed,
        AgentDriverEvent::ToolCallUpdated { call, .. }
            if call.status == AgentToolStatus::Completed
                && call.content.len() == 4
                && call.raw_output.as_deref() == Some("{\"ok\":true}")
    ));
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_session_advertises_the_digest_pinned_brokered_mcp_server() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s' \"$line\" > \"$ACP_CAPTURE\"; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-with-mcp\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let capture = temporary.path().join("session-new.json");
    let mcp = temporary.path().join("hyper-term-mcp");
    std::fs::write(&mcp, "fixture MCP").unwrap();
    let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&mcp, permissions).unwrap();
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: sha256_file(&executable).unwrap(),
        arguments: vec![],
        environment: BTreeMap::from([("ACP_CAPTURE".into(), capture.as_os_str().to_owned())]),
        implementation_version: "fixture-1".into(),
        provider_id: "fixture-acp".into(),
        workspace: workspace.path().to_owned(),
        brokered_mcp_server: Some(AcpMcpServerConfig {
            executable: mcp.clone(),
            executable_sha256: sha256_file(&mcp).unwrap(),
            arguments: vec![
                "--agent-mode".into(),
                "--socket".into(),
                "/tmp/hyperd.sock".into(),
            ],
            runtime_home: temporary.path().join("mcp-home"),
            runtime_temp: temporary.path().join("mcp-tmp"),
        }),
        containment: None,
        terminal_client: false,
    })
    .unwrap();

    client.initialize(Duration::from_secs(10)).unwrap();
    assert_eq!(
        client.start_session(Duration::from_secs(10)).unwrap(),
        "session-with-mcp"
    );
    let request: Value = serde_json::from_slice(&std::fs::read(&capture).unwrap()).unwrap();
    let server = &request["params"]["mcpServers"][0];
    assert_eq!(server["name"], "hyper_term");
    assert_eq!(
        server["command"].as_str(),
        mcp.canonicalize().unwrap().to_str()
    );
    assert_eq!(
        server["args"],
        json!(["--agent-mode", "--socket", "/tmp/hyperd.sock"])
    );
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_brokered_mcp_consent_is_correlated_and_forwarded_to_the_real_broker() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-mcp\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-mcp\",\"update\":{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"mcp-call-1\",\"kind\":\"execute\",\"title\":\"mcp.hyper_term.hyper_term.genui.compile\",\"status\":\"pending\",\"rawInput\":{\"server\":\"hyper_term\",\"tool\":\"hyper_term.genui.compile\",\"arguments\":{\"source\":\"export default function App() { return null }\"}},\"_meta\":{\"is_mcp_tool_call\":true}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"mcp-consent-1\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-mcp\",\"toolCall\":{\"toolCallId\":\"mcp-call-1\",\"kind\":\"execute\",\"status\":\"pending\"},\"_meta\":{\"is_mcp_tool_approval\":true},\"options\":[{\"optionId\":\"allow_once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"decline\",\"name\":\"Decline\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"mcp-consent-1\"'*'\"optionId\":\"allow_once\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let mcp = temporary.path().join("hyper-term-mcp");
    std::fs::write(&mcp, "fixture MCP").unwrap();
    let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&mcp, permissions).unwrap();
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: sha256_file(&executable).unwrap(),
        arguments: vec![],
        environment: BTreeMap::new(),
        implementation_version: "fixture-1".into(),
        provider_id: "fixture-acp".into(),
        workspace: workspace.path().to_owned(),
        brokered_mcp_server: Some(AcpMcpServerConfig {
            executable: mcp.clone(),
            executable_sha256: sha256_file(&mcp).unwrap(),
            arguments: vec!["--agent-mode".into()],
            runtime_home: temporary.path().join("mcp-home"),
            runtime_temp: temporary.path().join("mcp-tmp"),
        }),
        containment: None,
        terminal_client: false,
    })
    .unwrap();

    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client
        .start_turn(&session_id, "compile the counter")
        .unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ToolCallUpdated { call, .. }
            if call.title == "mcp.hyper_term.hyper_term.genui.compile"
    ));
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ProtocolNotice {
            method: Some(method),
            ..
        } if method == "session/request_permission"
    ));
    assert!(client.pending_effects().unwrap().is_empty());
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn provider_specific_mcp_shapes_are_bound_to_their_provider() {
    let claude_call: v1::ToolCall = serde_json::from_value(json!({
        "toolCallId": "provider-bound-call",
        "kind": "other",
        "title": "mcp__hyper_term__hyper_term_genui_compile",
        "status": "pending",
        "rawInput": {"entry": "App.tsx", "source": "export default null"}
    }))
    .unwrap();

    assert_eq!(
        brokered_mcp_tool("claude-acp", &claude_call),
        Some("hyper_term.genui.compile")
    );
    assert_eq!(brokered_mcp_tool("copilot-acp", &claude_call), None);

    let copilot_call: v1::ToolCall = serde_json::from_value(json!({
        "toolCallId": "copilot-provider-bound-call",
        "kind": "other",
        "title": "hyper_term-hyper_term-genui-compile",
        "status": "pending",
        "rawInput": {"entry": "App.tsx", "source": "export default null"}
    }))
    .unwrap();
    assert_eq!(
        brokered_mcp_tool("copilot-acp", &copilot_call),
        Some("hyper_term.genui.compile")
    );
    assert_eq!(brokered_mcp_tool("claude-acp", &copilot_call), None);
    assert_eq!(brokered_mcp_tool("fixture-acp", &copilot_call), None);
}

#[test]
fn claude_acp_brokered_mcp_consent_uses_the_explicit_tool_allowlist() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-claude-mcp\"}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"session-claude-mcp\",\"update\":{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"claude-mcp-call-1\",\"kind\":\"other\",\"title\":\"mcp__hyper_term__hyper_term_genui_compile\",\"status\":\"pending\",\"rawInput\":{\"entry\":\"App.tsx\",\"source\":\"export default function App() { return null }\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"claude-mcp-consent-1\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-claude-mcp\",\"toolCall\":{\"toolCallId\":\"claude-mcp-call-1\",\"kind\":\"other\",\"status\":\"pending\"},\"options\":[{\"optionId\":\"allow_once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"decline\",\"name\":\"Decline\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"claude-mcp-consent-1\"'*'\"optionId\":\"allow_once\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let mcp = temporary.path().join("hyper-term-mcp");
    std::fs::write(&mcp, "fixture MCP").unwrap();
    let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&mcp, permissions).unwrap();
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: sha256_file(&executable).unwrap(),
        arguments: vec![],
        environment: BTreeMap::new(),
        implementation_version: "fixture-claude".into(),
        provider_id: "claude-acp".into(),
        workspace: workspace.path().to_owned(),
        brokered_mcp_server: Some(AcpMcpServerConfig {
            executable: mcp.clone(),
            executable_sha256: sha256_file(&mcp).unwrap(),
            arguments: vec!["--agent-mode".into()],
            runtime_home: temporary.path().join("mcp-home"),
            runtime_temp: temporary.path().join("mcp-tmp"),
        }),
        containment: None,
        terminal_client: false,
    })
    .unwrap();

    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client
        .start_turn(&session_id, "compile the counter")
        .unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ToolCallUpdated { call, .. }
            if call.title == "mcp__hyper_term__hyper_term_genui_compile"
    ));
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::ProtocolNotice {
            method: Some(method),
            ..
        } if method == "session/request_permission"
    ));
    assert!(client.pending_effects().unwrap().is_empty());
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_unmatched_mcp_consent_remains_a_fail_closed_effect() {
    let (temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-mcp\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"unmatched-consent\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-mcp\",\"toolCall\":{\"toolCallId\":\"unseen-call\",\"kind\":\"execute\",\"status\":\"pending\"},\"_meta\":{\"is_mcp_tool_approval\":true},\"options\":[{\"optionId\":\"allow_once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"decline\",\"name\":\"Decline\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"unmatched-consent\"'*'\"optionId\":\"decline\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let mcp = temporary.path().join("hyper-term-mcp");
    std::fs::write(&mcp, "fixture MCP").unwrap();
    let mut permissions = std::fs::metadata(&mcp).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&mcp, permissions).unwrap();
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: sha256_file(&executable).unwrap(),
        arguments: vec![],
        environment: BTreeMap::new(),
        implementation_version: "fixture-1".into(),
        provider_id: "fixture-acp".into(),
        workspace: workspace.path().to_owned(),
        brokered_mcp_server: Some(AcpMcpServerConfig {
            executable: mcp.clone(),
            executable_sha256: sha256_file(&mcp).unwrap(),
            arguments: vec!["--agent-mode".into()],
            runtime_home: temporary.path().join("mcp-home"),
            runtime_temp: temporary.path().join("mcp-tmp"),
        }),
        containment: None,
        terminal_client: false,
    })
    .unwrap();

    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client
        .start_turn(&session_id, "attempt an unmatched call")
        .unwrap();
    let proposal = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::EffectProposed { proposal, .. } => proposal,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(proposal.kind, AgentEffectKind::Shell);
    client
        .resolve_effect(
            &proposal.request_id,
            AgentEffectAuthorization {
                operation_id: OperationId::new(),
                operation_revision: 1,
                proposal_sha256: proposal.payload_sha256,
                decision: PermissionDecision::RejectOnce,
            },
        )
        .unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[test]
fn acp_permission_becomes_brokered_proposal_and_rejection() {
    let (_temporary, executable) = fake_agent(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"session-2\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"permission-7\",\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"session-2\",\"toolCall\":{\"toolCallId\":\"tool-1\",\"kind\":\"execute\",\"title\":\"Run cargo test\"},\"options\":[{\"optionId\":\"allow-once\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"reject-once\",\"name\":\"Reject\",\"kind\":\"reject_once\"}]}}' ;;\n    *'\"id\":\"permission-7\"'*'\"optionId\":\"reject-once\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
    );
    let workspace = TempDir::new().unwrap();
    let client = launch(&executable, workspace.path());
    client.initialize(Duration::from_secs(10)).unwrap();
    let session_id = client.start_session(Duration::from_secs(10)).unwrap();
    client.start_turn(&session_id, "test it").unwrap();
    let proposal = match client.next_event(Duration::from_secs(2)).unwrap() {
        AgentDriverEvent::EffectProposed { proposal, .. } => proposal,
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(proposal.protocol, StructuredAgentProtocol::Acp);
    assert_eq!(proposal.kind, AgentEffectKind::Shell);
    assert_eq!(proposal.summary, "Run cargo test");
    client
        .resolve_effect(
            &proposal.request_id,
            AgentEffectAuthorization {
                operation_id: OperationId::new(),
                operation_revision: 1,
                proposal_sha256: proposal.payload_sha256,
                decision: PermissionDecision::RejectOnce,
            },
        )
        .unwrap();
    assert!(matches!(
        client.next_event(Duration::from_secs(2)).unwrap(),
        AgentDriverEvent::TurnCompleted { .. }
    ));
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}

#[cfg(target_os = "macos")]
#[test]
fn contained_acp_can_handshake_but_cannot_read_host_or_write_workspace() {
    use std::net::TcpListener;

    let root = TempDir::new().unwrap();
    let workspace = root.path().join("workspace");
    let scratch = root.path().join("scratch");
    let secret = root.path().join("host-secret.txt");
    let marker = scratch.join("boundary.txt");
    let forbidden = workspace.join("provider-write.txt");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(&secret, "must stay outside the ACP sandbox").unwrap();
    let script = format!(
        "#!/bin/sh\nif /bin/cat {secret} >/dev/null 2>&1; then host=allowed; else host=denied; fi\nif /usr/bin/touch {forbidden} >/dev/null 2>&1; then workspace=allowed; else workspace=denied; fi\nif [ \"$COPILOT_GITHUB_TOKEN\" = contained-provider-token ]; then credential=present; else credential=missing; fi\nprintf '%s,%s,%s\\n' \"$host\" \"$workspace\" \"$credential\" > {marker}\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":1,\"agentCapabilities\":{{}},\"authMethods\":[]}}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"sessionId\":\"contained-session\"}}}}' ;;\n  esac\ndone\n",
        secret = secret.display(),
        forbidden = forbidden.display(),
        marker = marker.display(),
    );
    let executable = root.path().join("contained-acp");
    std::fs::write(&executable, script).unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let credentialed_proxy_url =
        proxy_url.replacen("http://", "http://hyper-term:contained-test-token@", 1);
    let environment = BTreeMap::from([
        ("HOME".into(), scratch.clone().into_os_string()),
        ("PATH".into(), OsString::from("/usr/bin:/bin")),
        ("TERM".into(), OsString::from("dumb")),
        ("TMPDIR".into(), scratch.clone().into_os_string()),
    ]);
    let client = AcpAgentClient::launch(AcpAgentConfig {
        executable: executable.clone(),
        executable_sha256: sha256_file(&executable).unwrap(),
        arguments: Vec::new(),
        environment,
        implementation_version: "contained-fixture-1".into(),
        provider_id: "contained-fixture-acp".into(),
        workspace: workspace.canonicalize().unwrap(),
        brokered_mcp_server: None,
        containment: Some(AgentContainmentConfig {
            proxy_url,
            credentialed_proxy_url,
            allowed_hosts: vec!["api.example.com".into()],
            allowed_unix_sockets: Vec::new(),
            allowed_macos_mach_services: Vec::new(),
            credential_bindings: vec![AgentCredentialBinding {
                target_name: "COPILOT_GITHUB_TOKEN".into(),
                provider_id: "github-cli".into(),
                secret_id: "fixture".into(),
                audience: "contained-acp".into(),
                value: OsString::from("contained-provider-token"),
            }],
            read_paths: Vec::new(),
            write_paths: vec![scratch.canonicalize().unwrap()],
        }),
        terminal_client: false,
    })
    .unwrap();

    client.initialize(Duration::from_secs(10)).unwrap();
    assert_eq!(
        client.start_session(Duration::from_secs(10)).unwrap(),
        "contained-session"
    );
    assert_eq!(
        std::fs::read_to_string(marker).unwrap().trim(),
        "denied,denied,present"
    );
    let receipt = serde_json::to_string(client.context_receipt().unwrap()).unwrap();
    assert!(receipt.contains("managed-connect-proxy-session"));
    assert!(receipt.contains("github-cli"));
    assert!(!receipt.contains("contained-test-token"));
    assert!(!receipt.contains("contained-provider-token"));
    assert!(client.mcp_context_receipt().is_none());
    assert!(!forbidden.exists());
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}
