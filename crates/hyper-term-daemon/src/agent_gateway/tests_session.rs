    #[tokio::test(flavor = "multi_thread")]
    async fn agent_cancel_endpoint_interrupts_turn_and_keeps_session_reusable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"userAgent":"fake-codex"}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"gpt-test","displayName":"GPT Test","description":"Fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Medium"}],"defaultReasoningEffort":"medium","isDefault":true}]}}' ;;
    *'"method":"skills/list"'*) printf '%s\n' '{"id":3,"result":{"data":[]}}' ;;
    *'"method":"thread/start"'*) printf '%s\n' '{"id":4,"result":{"thread":{"id":"thread-cancel"}}}' ;;
    *'"method":"turn/start"'*) printf '%s\n' '{"id":5,"result":{"turn":{"id":"turn-cancel"}}}' ;;
    *'"method":"turn/interrupt"'*'"threadId":"thread-cancel"'*'"turnId":"turn-cancel"'*)
      printf '%s\n' '{"id":6,"result":{}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-cancel","turn":{"id":"turn-cancel","status":"interrupted"}}}' ;;
  esac
done
"#,
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: DaemonState::open(temporary.path().join("daemon-state")).unwrap(),
            provider_home: temporary.path().to_owned(),
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();

        assert_eq!(request(gateway.address(), &token, 4, "POST").await.0, 200);
        let turn_path = format!("/agent/session/turn?token={token}&session_id=4");
        assert_eq!(
            request_path(gateway.address(), &turn_path, "POST", b"Keep working")
                .await
                .0,
            StatusCode::ACCEPTED.as_u16()
        );
        loop {
            let (_, body) = request(gateway.address(), &token, 4, "GET").await;
            let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if snapshot["status"] == "running" && snapshot["turn_id"] == "turn-cancel" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let cancel_path = format!("/agent/session/cancel?token={token}&session_id=4");
        let (status, body) = request_path(gateway.address(), &cancel_path, "POST", b"").await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
            "cancelling"
        );
        loop {
            let (_, body) = request(gateway.address(), &token, 4, "GET").await;
            let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if snapshot["status"] == "completed" {
                assert!(snapshot["error"].is_null());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            request_path(gateway.address(), &cancel_path, "POST", b"")
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/session/cancel?token=wrong&session_id=4",
                "POST",
                b""
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        gateway.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn configured_acp_provider_uses_the_same_agent_session_projection() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let gateway_state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"acp-session-8\",\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"fast\",\"options\":[{\"value\":\"fast\",\"name\":\"Fast\"},{\"value\":\"deep\",\"name\":\"Deep\"}]}]}}' ;;\n    *'\"method\":\"session/set_config_option\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"configOptions\":[{\"id\":\"model\",\"name\":\"Model\",\"category\":\"model\",\"type\":\"select\",\"currentValue\":\"deep\",\"options\":[{\"value\":\"fast\",\"name\":\"Fast\"},{\"value\":\"deep\",\"name\":\"Deep\"}]}]}}' ;;\n    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"available_commands_update\",\"availableCommands\":[{\"name\":\"skills\",\"description\":\"Configure skills\"}]}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"user_message_chunk\",\"messageId\":\"5ee0f5a8-b508-4a0f-864d-9f69759b2087\",\"content\":{\"type\":\"text\",\"text\":\"Agent-injected user context.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Provider-neutral ACP is live.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Checking workspace\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-session-8\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Final answer.\"}}}}'\n      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        )
        .expect("fake ACP");
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: gateway_state.clone(),
            daemon,
            provider_home: temporary.path().to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session?token={token}&session_id=8&provider=fixture-acp"),
            "POST",
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["provider"], "fixture-acp");
        assert_eq!(response["protocol"], "acp-v1");
        assert_eq!(response["thread_id"], "acp-session-8");
        let (status, body) = request(gateway.address(), &token, 8, "GET").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot["capabilities"]["config_options"][0]["id"], "model");
        assert_eq!(
            snapshot["capabilities"]["config_options"][0]["kind"]["current_value"],
            "fast"
        );
        assert_eq!(
            snapshot["context"]["payload"]["type"],
            "agent_execution_context_recorded"
        );
        assert_eq!(
            snapshot["context"]["causation_id"],
            snapshot["context"]["correlation_id"]
        );
        assert_eq!(
            snapshot["context"]["payload"]["context"]["provider_id"],
            "fixture-acp"
        );
        assert_eq!(
            snapshot["context"]["payload"]["context"]["receipts"][0]["mode"],
            "hermetic"
        );
        assert_eq!(
            snapshot["context"]["payload"]["context"]["receipts"][0]["credential_bindings"][0]["reference"]
                ["secret_id"],
            "managed-connect-proxy-session"
        );
        assert!(!String::from_utf8_lossy(&body).contains("\"variables\""));
        let config_path = format!("/agent/session/config?token={token}&session_id=8");
        let config_request = serde_json::to_vec(&serde_json::json!({
            "config_id": "model",
            "value": {"type": "id", "value": "deep"}
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &config_path, "POST", &config_request).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            response["capabilities"]["config_options"][0]["kind"]["current_value"],
            "deep"
        );
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session?token={token}&session_id=8&provider=codex"),
                "POST",
                b"",
            )
            .await
            .0,
            StatusCode::CONFLICT.as_u16()
        );

        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/turn?token={token}&session_id=8"),
                "POST",
                b"Use ACP",
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let snapshot = loop {
            let (status, body) = request(gateway.address(), &token, 8, "GET").await;
            assert_eq!(status, StatusCode::OK.as_u16());
            let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if snapshot["status"] == "completed" {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let blocks = snapshot["document"]["blocks"]
            .as_array()
            .expect("snapshot blocks");
        assert_eq!(
            snapshot["capabilities"]["available_commands"][0]["name"],
            "skills"
        );
        let injected_user_message = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "user"
                    && block["payload"]["text"] == "Agent-injected user context."
                    && block["payload"]["external_message_id"]
                        == "5ee0f5a8-b508-4a0f-864d-9f69759b2087"
            })
            .expect("ACP user message update");
        let initial_message = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "agent"
                    && block["payload"]["text"] == "Provider-neutral ACP is live."
            })
            .expect("initial Agent message");
        let thought = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "thought"
                    && block["payload"]["text"] == "Checking workspace"
            })
            .expect("Agent thought");
        let final_message = blocks
            .iter()
            .position(|block| {
                block["payload"]["role"] == "agent" && block["payload"]["text"] == "Final answer."
            })
            .expect("final Agent message");
        assert!(injected_user_message < initial_message);
        assert!(initial_message < thought);
        assert!(thought < final_message);
        assert_eq!(
            std::fs::read_dir(gateway_state.join("agents"))
                .unwrap()
                .count(),
            1
        );
        gateway.shutdown().await.unwrap();
        assert_eq!(
            std::fs::read_dir(gateway_state.join("agents"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn full_gateway_restart_restores_agent_history_without_reusing_the_agent_process() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let daemon_state = temporary.path().join("daemon-state");
        let gateway_state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).unwrap();
        let fake_acp = temporary.path().join("fixture-acp-restart");
        std::fs::write(
            &fake_acp,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{},"authMethods":[],"agentInfo":{"name":"fixture-acp-restart","version":"1"}}}' ;;
    *'"method":"session/new"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fresh-provider-process"}}' ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fresh-provider-process","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Fresh provider answered."}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}' ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fake_acp, std::fs::Permissions::from_mode(0o700)).unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        let start_path = format!("/agent/session?token={token}&session_id=8&provider=fixture-acp");
        let turn_path = format!("/agent/session/turn?token={token}&session_id=8");

        let first = spawn_agent_gateway(restart_history_gateway_config(
            &workspace,
            &gateway_state,
            DaemonState::open(&daemon_state).unwrap(),
            temporary.path(),
            &fake_acp,
        ))
        .await
        .unwrap();
        let (_, body) = request_path(first.address(), &start_path, "POST", b"").await;
        let first_start: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(first_start["history_restored"], false);
        let task_id = first_start["task_id"].as_str().unwrap().to_owned();
        assert_eq!(
            request_path(
                first.address(),
                &turn_path,
                "POST",
                b"Remember this durable prompt",
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );
        loop {
            let (_, body) = request(first.address(), token, 8, "GET").await;
            let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if snapshot["status"] == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        first.shutdown().await.unwrap();

        let bindings = gateway_state.join(crate::agent_session_store::AGENT_SESSION_BINDING_FILE);
        assert_eq!(
            std::fs::metadata(&bindings).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let binding_json = std::fs::read_to_string(&bindings).unwrap();
        assert!(binding_json.contains(&task_id));
        assert!(!binding_json.contains("Remember this durable prompt"));

        let second = spawn_agent_gateway(restart_history_gateway_config(
            &workspace,
            &gateway_state,
            DaemonState::open(&daemon_state).unwrap(),
            temporary.path(),
            &fake_acp,
        ))
        .await
        .unwrap();
        let (_, body) = request_path(second.address(), &start_path, "POST", b"").await;
        let second_start: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(second_start["history_restored"], true);
        assert_eq!(second_start["task_id"], task_id);
        assert_eq!(second_start["thread_id"], "fresh-provider-process");

        let (_, body) = request(second.address(), token, 8, "GET").await;
        let restored: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(restored["history_restored"], true);
        assert!(restored["pending_operation_id"].is_null());
        assert!(
            restored["document"]["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["payload"]["text"] == "Remember this durable prompt")
        );

        let restart_path = format!(
            "/agent/session/restart?token={token}&session_id=8&provider=fixture-acp"
        );
        let (restart_status, restart_body) =
            request_path(second.address(), &restart_path, "POST", b"").await;
        assert_eq!(restart_status, StatusCode::OK.as_u16());
        let restarted: serde_json::Value = serde_json::from_slice(&restart_body).unwrap();
        assert_eq!(restarted["history_restored"], true);
        assert_eq!(restarted["task_id"], task_id);

        let (_, body) = request(second.address(), token, 8, "GET").await;
        let restarted_snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            restarted_snapshot["document"]["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["payload"]["text"] == "Remember this durable prompt")
        );

        assert_eq!(
            request_path(second.address(), &start_path, "DELETE", b"")
                .await
                .0,
            StatusCode::NO_CONTENT.as_u16()
        );
        second.shutdown().await.unwrap();
        let bindings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(bindings).unwrap()).unwrap();
        assert_eq!(bindings["entries"], serde_json::json!([]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn acp_artifact_workspace_apply_set_waits_for_one_exact_approval() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace_source = concat!(
            "export default function App() {\n",
            "  const title = 'Workspace title';\n",
            "  const keepOne = 1;\n",
            "  const keepTwo = 2;\n",
            "  const keepThree = 3;\n",
            "  const keepFour = 4;\n",
            "  const keepFive = 5;\n",
            "  const keepSix = 6;\n",
            "  const keepSeven = 7;\n",
            "  const keepEight = 8;\n",
            "  const footer = 'Workspace footer';\n",
            "  return <main>{title}{footer}</main>;\n",
            "}\n",
        );
        let workspace_theme = "export const accent = 'workspace';\n";
        std::fs::write(workspace.join("App.tsx"), workspace_source).unwrap();
        std::fs::write(workspace.join("theme.ts"), workspace_theme).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"workspace-apply-session\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace: workspace.clone(),
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            provider_home: temporary.path().to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let session_path =
            format!("/agent/session?token={token}&session_id=10&provider=fixture-acp");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(session["task_id"].clone()).unwrap();

        let seed = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Seed workspace apply fixture".into(),
                RiskClass::ReadOnly,
                vec!["artifact_build".into()],
            )
            .unwrap();
        let seed = daemon
            .decide_permission(
                task_id,
                seed.operation_id,
                seed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let seed = daemon
            .begin_operation(task_id, seed.operation_id, seed.revision)
            .unwrap();
        let artifact_source = concat!(
            "export default function App() {\n",
            "  const title = 'AI title';\n",
            "  const keepOne = 1;\n",
            "  const keepTwo = 2;\n",
            "  const keepThree = 3;\n",
            "  const keepFour = 4;\n",
            "  const keepFive = 5;\n",
            "  const keepSix = 6;\n",
            "  const keepSeven = 7;\n",
            "  const keepEight = 8;\n",
            "  const footer = 'AI footer';\n",
            "  return <main>{title}{footer}</main>;\n",
            "}\n",
        );
        let artifact_theme = "export const accent = 'agent';\n";
        let bundle = "globalThis.workspaceApplyFixture=true;";
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                seed.operation_id,
                seed.revision,
                GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 3,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([
                        ("/App.tsx".into(), artifact_source.into()),
                        ("/theme.ts".into(), artifact_theme.into()),
                    ]),
                    bundle: bundle.into(),
                    css: String::new(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: sha256_bytes(bundle.as_bytes()),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        daemon
            .complete_operation(
                task_id,
                seed.operation_id,
                seed.revision,
                OperationCompletion {
                    executor: "fixture".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: "seeded workspace apply fixture".into(),
                    result_digest: Some(accepted.content_digest.clone()),
                },
            )
            .unwrap();

        let editor_state_path = format!(
            "/agent/artifact/{}/editor-state?token={token}&session_id=10",
            accepted.artifact_id
        );
        let (status, body) = request_path(gateway.address(), &editor_state_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let baseline_state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(baseline_state["revision"], 0);
        assert_eq!(baseline_state["active_path"], "/App.tsx");
        assert_eq!(baseline_state["files"]["/theme.ts"], artifact_theme);
        let edited_theme = "export const accent = 'draft amber';\n";
        let checkpoint = serde_json::to_vec(&serde_json::json!({
            "expected_revision": 0,
            "base_source_revision": 3,
            "files": {
                "/App.tsx": artifact_source,
                "/theme.ts": edited_theme
            },
            "active_path": "/theme.ts",
            "view": "diff",
            "selections": {
                "/theme.ts": {"anchor": 7, "head": 12}
            }
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &editor_state_path, "PUT", &checkpoint).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let saved_state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved_state["revision"], 1);
        assert_eq!(saved_state["state_digest"].as_str().map(str::len), Some(64));
        let (status, body) = request_path(gateway.address(), &editor_state_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let restored_state: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(restored_state["files"]["/theme.ts"], edited_theme);
        assert_eq!(restored_state["active_path"], "/theme.ts");
        assert_eq!(restored_state["view"], "diff");
        assert_eq!(restored_state["selections"]["/theme.ts"]["head"], 12);
        assert_eq!(
            request_path(gateway.address(), &editor_state_path, "PUT", &checkpoint)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );

        let runtime_trace_path = format!(
            "/agent/artifact/{}/runtime-trace?token={token}&session_id=10",
            accepted.artifact_id
        );
        let trace_batch = serde_json::to_vec(&serde_json::json!({
            "source_revision": 3,
            "events": [
                {
                    "schema_version": 1,
                    "stream_id": "77777777-7777-4777-8777-777777777777",
                    "client_sequence": 1,
                    "kind": "checkpoint",
                    "name": "agent_status.changed",
                    "payload": {"expanded": true, "access_token": "must-not-persist"}
                },
                {
                    "schema_version": 1,
                    "stream_id": "77777777-7777-4777-8777-777777777777",
                    "client_sequence": 2,
                    "kind": "effect_receipt",
                    "name": "evidence.lookup",
                    "payload": {
                        "input": {"id": 7},
                        "outcome": "succeeded",
                        "output": {"passed": true}
                    }
                }
            ]
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &runtime_trace_path, "POST", &trace_batch).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let trace: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(trace["source_revision"], 3);
        assert_eq!(trace["events"].as_array().unwrap().len(), 2);
        assert_eq!(trace["projection_digest"].as_str().map(str::len), Some(64));
        assert_eq!(trace["events"][0]["event_sequence"], 1);
        assert_eq!(trace["events"][0]["redacted"], true);
        assert_eq!(trace["events"][0]["payload"]["access_token"], "[REDACTED]");
        assert_eq!(
            trace["events"][0]["payload_digest"].as_str().map(str::len),
            Some(64)
        );
        let (status, body) =
            request_path(gateway.address(), &runtime_trace_path, "POST", &trace_batch).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let (status, body) = request_path(gateway.address(), &runtime_trace_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["events"][0]["name"],
            "agent_status.changed"
        );
        let stale_trace = serde_json::to_vec(&serde_json::json!({
            "source_revision": 2,
            "events": [{
                "schema_version": 1,
                "stream_id": "88888888-8888-4888-8888-888888888888",
                "client_sequence": 1,
                "kind": "action",
                "name": "stale.action",
                "payload": null
            }]
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &runtime_trace_path, "POST", &stale_trace)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );

        let capsule_path = format!(
            "/agent/artifact/{}/debug-capsule?token={token}&session_id=10",
            accepted.artifact_id
        );
        let (status, body) = request_path(gateway.address(), &capsule_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let encoded_capsule = String::from_utf8(body.to_vec()).unwrap();
        assert!(!encoded_capsule.contains("draft amber"));
        assert!(!encoded_capsule.contains("must-not-persist"));
        assert!(encoded_capsule.contains("[REDACTED]"));
        let capsule: GenUiBugCapsule = serde_json::from_str(&encoded_capsule).unwrap();
        assert_eq!(capsule.mode, "replay_only");
        assert_eq!(capsule.editor.files.len(), 2);
        assert!(capsule.editor.files.iter().any(|file| file.modified));
        assert_eq!(capsule.runtime.events.len(), 2);
        assert_eq!(capsule.capsule_digest.as_deref().map(str::len), Some(64));
        assert!(
            crate::artifact_debug_capsule::verify_bug_capsule(&capsule).unwrap(),
            "serialized capsule must verify after an offline parse"
        );
        assert!(capsule.inventory.iter().any(|entry| {
            entry.category == "terminal_output"
                && entry.inclusion == hyper_term_protocol::GenUiBugCapsuleInclusion::Excluded
        }));

        let preview_path = format!(
            "/agent/artifact/{}/workspace-preview?token={token}&session_id=10",
            accepted.artifact_id
        );
        let preview_request = serde_json::to_vec(&serde_json::json!({
            "artifact_source_revision": 3,
            "mappings": [
                {"source_path": "/App.tsx", "target_path": "App.tsx"},
                {"source_path": "/theme.ts", "target_path": "theme.ts"}
            ]
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &preview_path, "POST", &preview_request).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let preview: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(preview["artifact_source_revision"], 3);
        assert_eq!(preview["review_digest"].as_str().map(str::len), Some(64));
        assert_eq!(preview["changes"].as_array().unwrap().len(), 2);
        assert_eq!(preview["changes"][0]["source_path"], "/App.tsx");
        assert_eq!(preview["changes"][0]["before"], workspace_source);
        assert_eq!(preview["changes"][0]["artifact_after"], artifact_source);
        assert_eq!(preview["changes"][0]["hunks"].as_array().unwrap().len(), 2);
        assert_eq!(preview["changes"][1]["source_path"], "/theme.ts");
        assert_eq!(preview["changes"][1]["hunks"].as_array().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            workspace_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            workspace_theme
        );

        let app_hunk_id = preview["changes"][0]["hunks"][0]["id"].as_str().unwrap();
        let theme_hunk_id = preview["changes"][1]["hunks"][0]["id"].as_str().unwrap();
        let invalid_selection = serde_json::to_vec(&serde_json::json!({
            "artifact_source_revision": 3,
            "review_digest": preview["review_digest"],
            "mappings": [
                {
                    "source_path": "/App.tsx",
                    "target_path": "App.tsx",
                    "hunk_ids": ["0".repeat(64)]
                },
                {
                    "source_path": "/theme.ts",
                    "target_path": "theme.ts",
                    "hunk_ids": []
                }
            ]
        }))
        .unwrap();
        let apply_path = format!(
            "/agent/artifact/{}/workspace-apply?token={token}&session_id=10",
            accepted.artifact_id
        );
        assert_eq!(
            request_path(gateway.address(), &apply_path, "POST", &invalid_selection,)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY.as_u16()
        );

        let apply_request = serde_json::to_vec(&serde_json::json!({
            "artifact_source_revision": 3,
            "review_digest": preview["review_digest"],
            "mappings": [
                {
                    "source_path": "/App.tsx",
                    "target_path": "App.tsx",
                    "hunk_ids": [app_hunk_id]
                },
                {
                    "source_path": "/theme.ts",
                    "target_path": "theme.ts",
                    "hunk_ids": [theme_hunk_id]
                }
            ]
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &apply_path, "POST", &apply_request).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let proposal: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let selected_app_source = workspace_source.replacen("Workspace title", "AI title", 1);
        assert_eq!(proposal["status"], "waiting_approval");
        assert_eq!(proposal["before"], workspace_source);
        assert_eq!(proposal["after"], selected_app_source);
        assert_eq!(proposal["base_digest"].as_str().map(str::len), Some(64));
        assert_eq!(
            proposal["transaction_digest"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(proposal["changes"].as_array().unwrap().len(), 2);
        assert_eq!(proposal["changes"][1]["source_path"], "/theme.ts");
        assert_eq!(proposal["changes"][1]["before"], workspace_theme);
        assert_eq!(proposal["changes"][1]["after"], artifact_theme);
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            workspace_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            workspace_theme
        );
        let operation_id: OperationId =
            serde_json::from_value(proposal["operation_id"].clone()).unwrap();
        let operation = daemon.operation(operation_id).unwrap();
        assert_eq!(operation.kind, OperationKind::FileEdit);
        assert_eq!(operation.risk, RiskClass::WorkspaceWrite);
        assert!(matches!(
            operation.action,
            OperationAction::Opaque { ref kind, .. } if kind == "hyper_term.workspace.apply"
        ));

        let stale_approval = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": proposal["operation_revision"].as_u64().unwrap() + 1,
            "approval_detail_digest": approval_digest(&daemon, operation_id),
            "decision": "allow_once"
        }))
        .unwrap();
        let permission_path = format!("/agent/session/permission?token={token}&session_id=10");
        assert_eq!(
            request_path(gateway.address(), &permission_path, "POST", &stale_approval,)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            workspace_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            workspace_theme
        );

        let approval = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": proposal["operation_revision"],
            "approval_detail_digest": approval_digest(&daemon, operation_id),
            "decision": "allow_once"
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &permission_path, "POST", &approval)
                .await
                .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let status_path = format!(
            "{apply_path}&operation_id={}",
            proposal["operation_id"].as_str().unwrap()
        );
        let applied = loop {
            let (status, body) = request_path(gateway.address(), &status_path, "GET", b"").await;
            assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
            let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if response["status"] != "applying" {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            selected_app_source
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("theme.ts")).unwrap(),
            artifact_theme
        );
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            hyper_term_protocol::OperationState::Succeeded
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
    async fn authenticated_acp_artifact_editor_queries_the_real_deno_lsp() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"editor-session\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let compiler_script = temporary.path().join("genui-compiler.js");
        let compiler_wasm = temporary.path().join("esbuild.wasm");
        let preview_shell = temporary.path().join("genui-preview.html");
        std::fs::write(&compiler_script, "compiler").unwrap();
        std::fs::write(&compiler_wasm, "wasm").unwrap();
        std::fs::write(
            &preview_shell,
            format!(
                "<html><head>{ARTIFACT_BOOTSTRAP_MARKER}<script>hyper_term_preview_boot</script></head></html>"
            ),
        )
        .unwrap();
        let deno =
            PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
                .canonicalize()
                .unwrap();
        assert_eq!(
            sha256_file(&deno).unwrap(),
            std::env::var("HYPER_TERM_DENO_SHA256").expect("HYPER_TERM_DENO_SHA256")
        );
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            provider_home: temporary.path().to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: Some(AgentGenUiRuntimeConfig {
                deno_executable: deno,
                runtime_version: "2.9.3".into(),
                compiler_script,
                compiler_wasm,
                preview_shell,
                compiler_version: "0.28.1".into(),
            }),
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session?token={token}&session_id=8&provider=fixture-acp"),
            "POST",
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(response["task_id"].clone()).unwrap();
        let proposed = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Compile editor LSP fixture".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                proposed.operation_id,
                proposed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let dispatching = daemon
            .begin_operation(task_id, proposed.operation_id, authorized.revision)
            .unwrap();
        let bundle = "globalThis.editorLsp = true;";
        let css = "";
        let mut digest = Sha256::new();
        digest.update(bundle.as_bytes());
        digest.update(css.as_bytes());
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                proposed.operation_id,
                dispatching.revision,
                hyper_term_protocol::GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 4,
                    entrypoint: "/main.ts".into(),
                    source_files: BTreeMap::from([
                        (
                            "/main.ts".into(),
                            "import { answer } from \"./value.ts\";\nconst result: string = answer;\n"
                                .into(),
                        ),
                        (
                            "/value.ts".into(),
                            "export const answer = \"ok\";\n".into(),
                        ),
                    ]),
                    bundle: bundle.into(),
                    css: css.into(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: digest
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        let lsp_path = format!(
            "/agent/artifact/{}/lsp?token={token}&session_id=8",
            accepted.artifact_id
        );
        let incomplete_draft = serde_json::to_vec(&serde_json::json!({
            "source_revision": 4,
            "document_path": "/main.ts",
            "draft_files": {
                "/main.ts": "export default 1;\n"
            },
            "kind": "diagnostics"
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &lsp_path, "POST", &incomplete_draft,)
                .await
                .0,
            StatusCode::BAD_REQUEST.as_u16()
        );
        let diagnostics = serde_json::to_vec(&serde_json::json!({
            "source_revision": 4,
            "document_path": "/main.ts",
            "draft_files": {
                "/main.ts": "import { answer } from \"./value.ts\";\nconst result: string = answer;\n",
                "/value.ts": "export const answer = 42;\n"
            },
            "kind": "diagnostics"
        }))
        .unwrap();
        let (status, body) = request_path(gateway.address(), &lsp_path, "POST", &diagnostics).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(response["diagnostics"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("not assignable to type 'string'"))
            })
        }));
        let completion = serde_json::to_vec(&serde_json::json!({
            "source_revision": 4,
            "document_path": "/main.ts",
            "draft_files": {
                "/main.ts": "const value = \"ok\";\nvalue.\n",
                "/value.ts": "export const answer = 42;\n"
            },
            "kind": "completion",
            "position": {"line": 1, "character": 6}
        }))
        .unwrap();
        let (status, body) = request_path(gateway.address(), &lsp_path, "POST", &completion).await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["document_version"], 2);
        assert!(
            response["completions"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires HYPER_TERM_DENO_PATH and built dist/runtime GenUI assets"]
    async fn approved_artifact_draft_is_recompiled_by_deno_and_replaces_the_revision() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_acp = temporary.path().join("fixture-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"draft-session\"}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_acp).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_acp, permissions).unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let deno =
            PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
                .canonicalize()
                .unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            provider_home: temporary.path().to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: fake_acp,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-1".into(),
            }],
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: Some(AgentGenUiRuntimeConfig {
                deno_executable: deno,
                runtime_version: "2.9.3".into(),
                compiler_script: repository.join("dist/runtime/genui-compiler.js"),
                compiler_wasm: repository.join("dist/runtime/esbuild.wasm"),
                preview_shell: repository.join("dist/runtime/genui/preview.html"),
                compiler_version: "0.28.1".into(),
            }),
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let session_path =
            format!("/agent/session?token={token}&session_id=9&provider=fixture-acp");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(session["task_id"].clone()).unwrap();
        let initial_operation = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Seed draft fixture".into(),
                RiskClass::ReadOnly,
                vec!["artifact_build".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                initial_operation.operation_id,
                initial_operation.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let dispatching = daemon
            .begin_operation(task_id, initial_operation.operation_id, authorized.revision)
            .unwrap();
        let initial_source = "export default function App(){return <main>Initial</main>;}";
        let bundle = "globalThis.initialArtifact=true;";
        let accepted = daemon
            .accept_genui_artifact(
                task_id,
                initial_operation.operation_id,
                dispatching.revision,
                GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 1,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([("/App.tsx".into(), initial_source.into())]),
                    bundle: bundle.into(),
                    css: String::new(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: sha256_bytes(bundle.as_bytes()),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        daemon
            .complete_operation(
                task_id,
                initial_operation.operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "fixture".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: "seeded fixture".into(),
                    result_digest: Some(accepted.content_digest.clone()),
                },
            )
            .unwrap();
        let edited_source = "export default function App(){return <main>Published by Deno</main>;}";
        let draft_path = format!(
            "/agent/artifact/{}/draft?token={token}&session_id=9",
            accepted.artifact_id
        );
        let draft_body = serde_json::to_vec(&serde_json::json!({
            "base_source_revision": 1,
            "entrypoint": "/App.tsx",
            "files": {"/App.tsx": edited_source}
        }))
        .unwrap();
        let (status, body) =
            request_path(gateway.address(), &draft_path, "POST", &draft_body).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let rejected: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let rejected_operation: OperationId =
            serde_json::from_value(rejected["operation_id"].clone()).unwrap();
        let rejection = serde_json::to_vec(&serde_json::json!({
            "operation_id": rejected_operation,
            "expected_revision": rejected["operation_revision"],
            "approval_detail_digest": approval_digest(&daemon, rejected_operation),
            "decision": "reject_once"
        }))
        .unwrap();
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/permission?token={token}&session_id=9"),
                "POST",
                &rejection,
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let rejected_path = format!("{draft_path}&operation_id={rejected_operation}");
        let (status, body) = request_path(gateway.address(), &rejected_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
            "rejected"
        );
        assert_eq!(
            daemon.active_genui_artifact(task_id).unwrap().unwrap(),
            accepted
        );
        let (status, body) =
            request_path(gateway.address(), &draft_path, "POST", &draft_body).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let proposed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(proposed["status"], "waiting_approval");
        let operation_id: OperationId =
            serde_json::from_value(proposed["operation_id"].clone()).unwrap();
        let operation_revision = proposed["operation_revision"].as_u64().unwrap();
        let snapshot = serde_json::to_string(&daemon.block_snapshot(task_id).unwrap()).unwrap();
        assert!(snapshot.contains("\"type\":\"approval\""));
        assert!(snapshot.contains(&operation_id.to_string()));
        let permission = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "approval_detail_digest": approval_digest(&daemon, operation_id),
            "decision": "allow_once"
        }))
        .unwrap();
        let (status, body) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=9"),
            "POST",
            &permission,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let status_path = format!("{draft_path}&operation_id={operation_id}");
        let mut published = None;
        for _ in 0..200 {
            let (status, body) = request_path(gateway.address(), &status_path, "GET", b"").await;
            assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
            let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if response["status"] == "accepted" {
                published = Some(response);
                break;
            }
            assert!(matches!(
                response["status"].as_str(),
                Some("waiting_approval" | "compiling")
            ));
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let published = published.expect("Deno artifact draft accepted within five seconds");
        let published_id = published["artifact"]["artifact_id"].as_str().unwrap();
        assert_eq!(published["artifact"]["source_revision"], 2);
        let source_path =
            format!("/agent/artifact/{published_id}/source?token={token}&session_id=9");
        let (status, body) = request_path(gateway.address(), &source_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let source: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(source["files"]["/App.tsx"], edited_source);
        assert_eq!(source["source_revision"], 2);
        let stale = serde_json::to_vec(&serde_json::json!({
            "base_source_revision": 1,
            "entrypoint": "/App.tsx",
            "files": {"/App.tsx": "export default () => null;"}
        }))
        .unwrap();
        let stale_path = format!("/agent/artifact/{published_id}/draft?token={token}&session_id=9");
        assert_eq!(
            request_path(gateway.address(), &stale_path, "POST", &stale)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );
        gateway.shutdown().await.unwrap();
    }
