    #[tokio::test(flavor = "multi_thread")]
    async fn accepted_artifact_preview_is_authenticated_current_and_network_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let gateway_state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"model/list\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[{\"model\":\"gpt-test\",\"displayName\":\"GPT Test\",\"description\":\"Fixture\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"medium\",\"description\":\"Medium\"}],\"defaultReasoningEffort\":\"medium\",\"isDefault\":true}]}}' ;;\n    *'\"method\":\"skills/list\"'*) printf '%s\\n' '{\"id\":3,\"result\":{\"data\":[]}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":4,\"result\":{\"thread\":{\"id\":\"preview-thread\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).unwrap();

        let deno = temporary.path().join("deno");
        let compiler_script = temporary.path().join("genui-compiler.js");
        let compiler_wasm = temporary.path().join("esbuild.wasm");
        let preview_shell = temporary.path().join("genui-preview.html");
        std::fs::write(&deno, "deno").unwrap();
        std::fs::write(&compiler_script, "compiler").unwrap();
        std::fs::write(&compiler_wasm, "wasm").unwrap();
        std::fs::write(
            &preview_shell,
            format!(
                "<html><head>{ARTIFACT_BOOTSTRAP_MARKER}<script>hyper_term_preview_boot</script></head><body></body></html>"
            ),
        )
        .unwrap();
        let workbench_assets = temporary.path().join("workbench");
        std::fs::create_dir_all(workbench_assets.join("genui")).unwrap();
        std::fs::write(
            workbench_assets.join("index.html"),
            "<html><body>trusted-workbench<script src=\"index.js\"></script></body></html>",
        )
        .unwrap();
        std::fs::write(
            workbench_assets.join("index.js"),
            "globalThis.workbench=true;",
        )
        .unwrap();
        std::fs::write(
            workbench_assets.join("genui/preview.html"),
            "<html><script>globalThis.preview=true;</script></html>",
        )
        .unwrap();

        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: gateway_state,
            daemon: daemon.clone(),
            provider_home: temporary.path().to_owned(),
            codex_executable: Some(fake_codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
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
            workbench_assets: Some(workbench_assets),
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");

        let (status, body) = request(gateway.address(), &token, 6, "POST").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(response["task_id"].clone()).unwrap();
        let workbench_path =
            format!("/agent/workbench/?token={token}&session_id=6&surface=artifact");
        let (status, headers, body) =
            request_path_raw(gateway.address(), &workbench_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-type: text/html"));
        assert!(headers.contains("connect-src 'self'"));
        assert!(headers.contains("'wasm-unsafe-eval'"));
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("trusted-workbench")
        );
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/workbench?token=wrong&session_id=6",
                "GET",
                b"",
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let (status, headers, body) =
            request_path_raw(gateway.address(), "/agent/workbench/index.js", "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert!(
            String::from_utf8(headers)
                .unwrap()
                .to_ascii_lowercase()
                .contains("content-type: text/javascript")
        );
        assert_eq!(body, b"globalThis.workbench=true;");
        let (status, headers, _) = request_path_raw(
            gateway.address(),
            "/agent/workbench/genui/preview.html",
            "GET",
            b"",
        )
        .await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("frame-ancestors 'self'"));
        assert!(headers.contains("connect-src 'none'"));
        let proposed = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Compile preview fixture".into(),
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
        let bundle = "globalThis.__HYPER_PREVIEW_PROBE__ = 'ready';";
        let css = "main{color:#d7ff72}";
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
                    source_revision: 9,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([(
                        "/App.tsx".into(),
                        "export default () => <main>ready</main>;".into(),
                    )]),
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

        let preview_path = format!(
            "/agent/artifact/{}/preview?token={token}&session_id=6",
            accepted.artifact_id
        );
        let (status, headers, body) =
            request_path_raw(gateway.address(), &preview_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-security-policy:"));
        assert!(headers.contains("connect-src 'none'"));
        assert!(headers.contains("cache-control: no-store"));
        let document = String::from_utf8(body).unwrap();
        assert!(document.contains("__HYPER_PREVIEW_PROBE__"));
        assert!(document.contains(&accepted.artifact_id.to_string()));
        assert!(!document.contains(ARTIFACT_BOOTSTRAP_MARKER));

        let source_map_path = format!(
            "/agent/artifact/{}/source-map?token={token}&session_id=6",
            accepted.artifact_id
        );
        let (status, source_map) =
            request_path(gateway.address(), &source_map_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert_eq!(source_map, b"{\"version\":3}");
        let source_path = format!(
            "/agent/artifact/{}/source?token={token}&session_id=6",
            accepted.artifact_id
        );
        let (status, headers, source) =
            request_path_raw(gateway.address(), &source_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
        assert!(headers.contains("content-type: application/json"));
        assert!(headers.contains("cache-control: no-store"));
        let source: serde_json::Value = serde_json::from_slice(&source).unwrap();
        assert_eq!(source["artifact_id"], accepted.artifact_id.to_string());
        assert_eq!(source["source_revision"], 9);
        assert_eq!(source["entrypoint"], "/App.tsx");
        assert_eq!(
            source["files"]["/App.tsx"],
            "export default () => <main>ready</main>;"
        );
        let lsp_path = format!(
            "/agent/artifact/{}/lsp?token={token}&session_id=6",
            accepted.artifact_id
        );
        let lsp_request = serde_json::to_vec(&serde_json::json!({
            "source_revision": 9,
            "document_path": "/App.tsx",
            "draft_files": {
                "/App.tsx": "export default () => <main>ready</main>;"
            },
            "kind": "diagnostics"
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &lsp_path, "POST", &lsp_request,)
                .await
                .0,
            StatusCode::FORBIDDEN.as_u16()
        );
        let unauthorized_source_path = format!(
            "/agent/artifact/{}/source?token=wrong&session_id=6",
            accepted.artifact_id
        );
        assert_eq!(
            request_path(gateway.address(), &unauthorized_source_path, "GET", b"")
                .await
                .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let next_operation = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "b".repeat(64),
                },
                "Compile second preview fixture".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let next_authorized = daemon
            .decide_permission(
                task_id,
                next_operation.operation_id,
                next_operation.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        let next_dispatching = daemon
            .begin_operation(
                task_id,
                next_operation.operation_id,
                next_authorized.revision,
            )
            .unwrap();
        let next_bundle = "globalThis.__HYPER_PREVIEW_PROBE__ = 'second';";
        let next = daemon
            .accept_genui_artifact_from_base(
                task_id,
                next_operation.operation_id,
                next_dispatching.revision,
                accepted.artifact_id,
                accepted.source_revision,
                hyper_term_protocol::GenUiArtifactCandidate {
                    schema_version: 1,
                    source_revision: 10,
                    entrypoint: "/App.tsx".into(),
                    source_files: BTreeMap::from([(
                        "/App.tsx".into(),
                        "export default () => <main>second</main>;".into(),
                    )]),
                    bundle: next_bundle.into(),
                    css: String::new(),
                    source_map: "{\"version\":3}".into(),
                    content_digest: sha256_bytes(next_bundle.as_bytes()),
                    compiler: hyper_term_protocol::GenUiCompilerIdentity {
                        name: "esbuild-wasm".into(),
                        version: "0.28.1".into(),
                    },
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        let history_path = format!(
            "/agent/artifact/{}/history?token={token}&session_id=6",
            next.artifact_id
        );
        let (status, history) = request_path(gateway.address(), &history_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let history: serde_json::Value = serde_json::from_slice(&history).unwrap();
        assert_eq!(history["active_artifact_id"], next.artifact_id.to_string());
        assert_eq!(history["entries"].as_array().unwrap().len(), 2);
        assert_eq!(
            history["entries"][0]["artifact"]["artifact_id"],
            next.artifact_id.to_string()
        );
        assert_eq!(
            history["entries"][1]["artifact"]["artifact_id"],
            accepted.artifact_id.to_string()
        );
        let historical_source_path = format!(
            "/agent/artifact/{}/history/{}/source?token={token}&session_id=6",
            next.artifact_id, accepted.artifact_id
        );
        let (status, historical_source) =
            request_path(gateway.address(), &historical_source_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let historical_source: serde_json::Value =
            serde_json::from_slice(&historical_source).unwrap();
        assert_eq!(historical_source["source_revision"], 9);
        assert_eq!(
            historical_source["files"]["/App.tsx"],
            "export default () => <main>ready</main>;"
        );
        let stale_history_path = format!(
            "/agent/artifact/{}/history?token={token}&session_id=6",
            accepted.artifact_id
        );
        assert_eq!(
            request_path(gateway.address(), &stale_history_path, "GET", b"")
                .await
                .0,
            StatusCode::NOT_FOUND.as_u16()
        );
        let stale_path = format!(
            "/agent/artifact/{}/preview?token={token}&session_id=6",
            ArtifactId::new()
        );
        assert_eq!(
            request_path(gateway.address(), &stale_path, "GET", b"")
                .await
                .0,
            StatusCode::NOT_FOUND.as_u16()
        );

        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    async fn acp_terminal_create_requires_approval_and_runs_only_in_tier2() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        run_git(&workspace, &["init", "-q"]);
        run_git(&workspace, &["config", "user.name", "Hyper Term Test"]);
        run_git(
            &workspace,
            &["config", "user.email", "hyper-term@example.invalid"],
        );
        std::fs::write(workspace.join("README.md"), "source\n").expect("fixture");
        run_git(&workspace, &["add", "."]);
        run_git(&workspace, &["commit", "-qm", "fixture"]);
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_acp = temporary.path().join("fixture-terminal-acp");
        std::fs::write(
            &fake_acp,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*'\"terminal\":true'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[]}}' ;;\n    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"terminal-session\"}}' ;;\n    *'\"method\":\"session/prompt\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-create-1\",\"method\":\"terminal/create\",\"params\":{\"sessionId\":\"terminal-session\",\"command\":\"printf\",\"args\":[\"tier2-output\\\\n\"],\"outputByteLimit\":4096}}' ;;\n    *'\"id\":\"terminal-create-1\"'*'\"terminalId\"'*) terminal_id=$(printf '%s' \"$line\" | sed -n 's/.*\"terminalId\":\"\\([^\"]*\\)\".*/\\1/p'); printf '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-output-1\",\"method\":\"terminal/output\",\"params\":{\"sessionId\":\"terminal-session\",\"terminalId\":\"%s\"}}\\n' \"$terminal_id\" ;;\n    *'\"id\":\"terminal-output-1\"'*) printf '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-wait-1\",\"method\":\"terminal/wait_for_exit\",\"params\":{\"sessionId\":\"terminal-session\",\"terminalId\":\"%s\"}}\\n' \"$terminal_id\" ;;\n    *'\"id\":\"terminal-wait-1\"'*) printf '{\"jsonrpc\":\"2.0\",\"id\":\"terminal-release-1\",\"method\":\"terminal/release\",\"params\":{\"sessionId\":\"terminal-session\",\"terminalId\":\"%s\"}}\\n' \"$terminal_id\" ;;\n    *'\"id\":\"terminal-release-1\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"terminal-session\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Tier 2 terminal completed.\"}}}}' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"stopReason\":\"end_turn\"}}' ;;\n  esac\ndone\n",
        )
        .expect("fake ACP");
        std::fs::set_permissions(&fake_acp, std::fs::Permissions::from_mode(0o700))
            .expect("fake ACP executable");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace: workspace.clone(),
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            provider_home: temporary.path().to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-terminal-acp".into(),
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
            tier2_runner: Some(fake_lima_runner(temporary.path())),
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .expect("agent gateway");
        let session_path =
            format!("/agent/session?token={token}&session_id=12&provider=fixture-terminal-acp");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        let started: serde_json::Value = serde_json::from_slice(&body).expect("start response");
        let task_id: TaskId = serde_json::from_value(started["task_id"].clone()).expect("task id");
        let turn_path = format!("/agent/session/turn?token={token}&session_id=12");
        assert_eq!(
            request_path(
                gateway.address(),
                &turn_path,
                "POST",
                b"Run the bounded terminal"
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );

        let (operation_id, operation_revision) =
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let (status, body) = request(gateway.address(), &token, 12, "GET").await;
                    assert_eq!(status, StatusCode::OK.as_u16());
                    let snapshot: serde_json::Value =
                        serde_json::from_slice(&body).expect("snapshot");
                    assert_ne!(snapshot["status"], "failed", "{snapshot:#}");
                    if snapshot["status"] == "waiting_approval" {
                        let approval = snapshot["document"]["blocks"]
                            .as_array()
                            .expect("blocks")
                            .iter()
                            .find(|block| block["kind"] == "approval")
                            .expect("approval block");
                        break (
                            approval["payload"]["operation_id"]
                                .as_str()
                                .expect("operation id")
                                .to_owned(),
                            approval["payload"]["operation_revision"]
                                .as_u64()
                                .expect("operation revision"),
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("ACP terminal did not reach approval");
        let operation_uuid = uuid::Uuid::parse_str(&operation_id).expect("operation UUID");
        let operation_id = OperationId::from_uuid(operation_uuid);
        let operation = daemon.operation(operation_id).expect("terminal operation");
        assert_eq!(operation.revision, operation_revision);
        assert_eq!(operation.kind, OperationKind::Shell);
        assert_eq!(operation.risk, RiskClass::ExternalEffect);
        assert!(
            operation
                .required_capabilities
                .iter()
                .any(|capability| capability == "sandbox.isolated_task")
        );
        assert!(!workspace.join("generated.txt").exists());

        let approval = serde_json::to_vec(&serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "decision": "allow_once"
        }))
        .expect("approval");
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/permission?token={token}&session_id=12"),
                "POST",
                &approval,
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );
        let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (status, body) = request(gateway.address(), &token, 12, "GET").await;
                assert_eq!(status, StatusCode::OK.as_u16());
                let snapshot: serde_json::Value = serde_json::from_slice(&body).expect("snapshot");
                assert_ne!(snapshot["status"], "failed", "{snapshot:#}");
                if snapshot["status"] == "completed" {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("ACP terminal turn did not complete");
        assert!(
            snapshot["document"]["blocks"]
                .as_array()
                .expect("blocks")
                .iter()
                .any(|block| block["payload"]["text"] == "Tier 2 terminal completed.")
        );
        assert_eq!(
            daemon
                .operation(operation_id)
                .expect("completed operation")
                .state,
            OperationState::Succeeded
        );
        let retained = daemon
            .isolated_result_reviews(task_id)
            .expect("Tier 2 results");
        assert_eq!(retained.len(), 1);
        assert!(retained[0].receipt.stdout.contains("tier2-output"));
        assert!(!workspace.join("generated.txt").exists());
        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proposal_only_agent_can_reject_an_effect_and_finish_the_turn() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"model/list\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[{\"model\":\"gpt-test\",\"displayName\":\"GPT Test\",\"description\":\"Fixture\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"medium\",\"description\":\"Medium\"}],\"defaultReasoningEffort\":\"medium\",\"isDefault\":true}]}}' ;;\n    *'\"method\":\"skills/list\"'*) printf '%s\\n' '{\"id\":3,\"result\":{\"data\":[]}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":4,\"result\":{\"thread\":{\"id\":\"thread-4\"}}}' ;;\n    *'\"method\":\"turn/start\"'*)\n      printf '%s\\n' '{\"id\":5,\"result\":{\"turn\":{\"id\":\"turn-2\"}}}'\n      printf '%s\\n' '{\"id\":77,\"method\":\"item/commandExecution/requestApproval\",\"params\":{\"threadId\":\"thread-4\",\"turnId\":\"turn-2\",\"itemId\":\"command-1\",\"command\":\"touch forbidden\"}}' ;;\n    *'\"id\":77'*'\"decision\":\"decline\"'*)\n      printf '%s\\n' '{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread-4\",\"turnId\":\"turn-2\",\"itemId\":\"message-2\",\"delta\":\"The command was rejected.\"}}'\n      printf '%s\\n' '{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread-4\",\"turn\":{\"id\":\"turn-2\",\"status\":\"completed\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex)
            .expect("fake Codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).expect("fake Codex executable");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace,
            state_directory: state,
            daemon,
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
        .expect("agent gateway");

        assert_eq!(
            request(gateway.address(), &token, 4, "POST").await.0,
            StatusCode::OK.as_u16()
        );
        assert_eq!(
            request_path(
                gateway.address(),
                &format!("/agent/session/turn?token={token}&session_id=4"),
                "POST",
                b"Try a command",
            )
            .await
            .0,
            StatusCode::ACCEPTED.as_u16()
        );

        let (operation_id, operation_revision) =
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let (status, body) = request(gateway.address(), &token, 4, "GET").await;
                    assert_eq!(status, StatusCode::OK.as_u16());
                    let snapshot: serde_json::Value =
                        serde_json::from_slice(&body).expect("snapshot response");
                    assert_ne!(
                        snapshot["status"], "failed",
                        "Agent failed before approval: {snapshot}"
                    );
                    if snapshot["status"] == "waiting_approval" {
                        let approval = snapshot["document"]["blocks"]
                            .as_array()
                            .expect("snapshot blocks")
                            .iter()
                            .find(|block| block["kind"] == "approval")
                            .expect("approval block");
                        break (
                            approval["payload"]["operation_id"]
                                .as_str()
                                .expect("operation id")
                                .to_owned(),
                            approval["payload"]["operation_revision"]
                                .as_u64()
                                .expect("operation revision"),
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Agent did not reach waiting approval");
        let unsafe_decision = serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "decision": "allow_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=4"),
            "POST",
            &serde_json::to_vec(&unsafe_decision).expect("unsafe permission decision"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN.as_u16());

        let decision = serde_json::json!({
            "operation_id": operation_id,
            "expected_revision": operation_revision,
            "decision": "reject_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=4"),
            "POST",
            &serde_json::to_vec(&decision).expect("permission decision"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());

        let snapshot = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (status, body) = request(gateway.address(), &token, 4, "GET").await;
                assert_eq!(status, StatusCode::OK.as_u16());
                let snapshot: serde_json::Value =
                    serde_json::from_slice(&body).expect("snapshot response");
                if snapshot["status"] == "completed" {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Agent did not complete after rejection");
        let blocks = snapshot["document"]["blocks"]
            .as_array()
            .expect("snapshot blocks");
        assert!(blocks.iter().any(|block| {
            block["kind"] == "operation" && block["payload"]["state"] == "cancelled"
        }));
        assert!(blocks.iter().any(|block| {
            block["kind"] == "approval"
                && block["payload"]["decision"] == "reject_once"
                && block["actions"].as_array().is_some_and(Vec::is_empty)
        }));
        assert!(blocks.iter().any(|block| {
            block["payload"]["role"] == "agent"
                && block["payload"]["text"] == "The command was rejected."
        }));

        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_known_read_only_mcp_operations_can_be_allowed_from_agent_chrome() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"model/list\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[{\"model\":\"gpt-test\",\"displayName\":\"GPT Test\",\"description\":\"Fixture\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"medium\",\"description\":\"Medium\"}],\"defaultReasoningEffort\":\"medium\",\"isDefault\":true}]}}' ;;\n    *'\"method\":\"skills/list\"'*) printf '%s\\n' '{\"id\":3,\"result\":{\"data\":[]}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":4,\"result\":{\"thread\":{\"id\":\"thread-mcp\"}}}' ;;\n  esac\ndone\n",
        )
        .expect("fake Codex");
        let mut permissions = std::fs::metadata(&fake_codex)
            .expect("fake Codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).expect("fake Codex executable");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace,
            state_directory: state,
            daemon: daemon.clone(),
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
        .expect("agent gateway");

        let (status, body) = request(gateway.address(), &token, 5, "POST").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).expect("start response");
        let task_id = TaskId::from_uuid(
            uuid::Uuid::parse_str(response["task_id"].as_str().expect("task id"))
                .expect("task UUID"),
        );
        let mcp = daemon
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
            .expect("MCP proposal");
        let allow = serde_json::json!({
            "operation_id": mcp.operation_id,
            "expected_revision": mcp.revision,
            "decision": "allow_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=5"),
            "POST",
            &serde_json::to_vec(&allow).expect("allow decision"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        assert_eq!(
            daemon
                .operation(mcp.operation_id)
                .expect("MCP operation")
                .state,
            hyper_term_protocol::OperationState::Authorized
        );

        let opaque = daemon
            .propose_operation(
                task_id,
                OperationKind::Other("agent_shell".into()),
                OperationAction::Opaque {
                    kind: "item/commandExecution/requestApproval".into(),
                    payload_digest: "b".repeat(64),
                },
                "touch forbidden".into(),
                RiskClass::ExternalEffect,
                vec!["shell".into()],
            )
            .expect("opaque proposal");
        let unsafe_allow = serde_json::json!({
            "operation_id": opaque.operation_id,
            "expected_revision": opaque.revision,
            "decision": "allow_once"
        });
        let (status, _) = request_path(
            gateway.address(),
            &format!("/agent/session/permission?token={token}&session_id=5"),
            "POST",
            &serde_json::to_vec(&unsafe_allow).expect("unsafe allow decision"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN.as_u16());

        gateway.shutdown().await.expect("shutdown gateway");
    }

    #[test]
    fn brokered_mcp_keeps_deno_paths_outside_the_agent_process_tree() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state_directory = temporary.path().join("gateway-state");
        std::fs::create_dir_all(workspace.join("src")).expect("workspace");
        std::fs::create_dir_all(workspace.join("node_modules/ignored")).expect("dependencies");
        std::fs::write(
            workspace.join("src/main.ts"),
            "export const answer: number = 42;\n",
        )
        .expect("source");
        std::fs::write(
            workspace.join("node_modules/ignored/index.ts"),
            "export const ignored = true;\n",
        )
        .expect("generated dependency");
        std::fs::create_dir_all(&state_directory).expect("state directory");
        let mcp = temporary.path().join("hyper-term-mcp");
        let deno = temporary.path().join("deno");
        let script = temporary.path().join("genui-compiler.js");
        let wasm = temporary.path().join("esbuild.wasm");
        let preview = temporary.path().join("genui-preview.html");
        std::fs::write(&mcp, "mcp").expect("mcp");
        std::fs::write(&deno, "deno").expect("deno");
        std::fs::write(&script, "compiler").expect("compiler");
        std::fs::write(&wasm, "wasm").expect("wasm");
        std::fs::write(
            &preview,
            "<!-- HYPER_TERM_ARTIFACT_BOOTSTRAP -->hyper_term_preview_boot",
        )
        .expect("preview");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let runtime = AgentGatewayRuntime {
            config: Arc::new(AgentGatewayConfig {
                bind: "127.0.0.1:0".parse().expect("bind"),
                token: "0123456789abcdef0123456789abcdef".into(),
                workspace: workspace.canonicalize().unwrap(),
                state_directory: state_directory.canonicalize().unwrap(),
                daemon: daemon.clone(),
                provider_home: temporary.path().to_owned(),
                codex_executable: None,
                codex_auth_file: None,
                acp_providers: Vec::new(),
                local_mcp_servers: Vec::new(),
                mcp_executable: Some(mcp),
                genui_runtime: Some(AgentGenUiRuntimeConfig {
                    deno_executable: deno,
                    runtime_version: "2.9.3".into(),
                    compiler_script: script,
                    compiler_wasm: wasm,
                    preview_shell: preview,
                    compiler_version: "0.28.1".into(),
                }),
                workbench_assets: None,
                debug_capsule: None,
                tier2_runner: None,
                control_socket: temporary.path().join("hyperd.sock"),
            }),
            local_mcp: Arc::new(LocalMcpRuntimeManager::new(daemon.clone(), Vec::new()).unwrap()),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            session_bindings: Arc::new(AgentSessionBindingStore::open(&state_directory).unwrap()),
            preview_shell: None,
            workbench_assets: None,
            editor_lsp: None,
            artifact_draft_compiler: None,
            artifact_editor_store: Arc::new(ArtifactEditorStore::open(&state_directory).unwrap()),
            artifact_editor_lock: Arc::new(Mutex::new(())),
            artifact_runtime_trace_store: Arc::new(
                ArtifactRuntimeTraceStore::open(&state_directory).unwrap(),
            ),
            artifact_runtime_trace_lock: Arc::new(Mutex::new(())),
            artifact_drafts: Arc::new(Mutex::new(HashMap::new())),
            workspace_applies: Arc::new(Mutex::new(HashMap::new())),
            workspace_recovery_block: Arc::new(Mutex::new(None)),
        };
        let session_root = state_directory.join("agents/session-7");
        let task_id = daemon.create_task("Agent MCP boundary".into()).unwrap();
        let config = runtime
            .mcp_launch(task_id, &session_root)
            .expect("MCP configured")
            .expect("valid MCP config");
        let arguments = config
            .arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(arguments.contains(&std::borrow::Cow::Borrowed("--enable-genui")));
        assert!(arguments.contains(&std::borrow::Cow::Borrowed("--enable-deno-lsp")));
        for forbidden in [
            "--deno",
            "--deno-sha256",
            "--workspace-snapshot",
            "--genui-script",
            "--genui-wasm",
        ] {
            assert!(!arguments.contains(&std::borrow::Cow::Borrowed(forbidden)));
        }
        assert!(!arguments.iter().any(|argument| argument.len() == 64));
        let snapshot = state_directory
            .join("brokered-mcp")
            .join(task_id.to_string())
            .join("workspace-snapshot");
        assert_eq!(
            std::fs::read_to_string(snapshot.join("src/main.ts")).unwrap(),
            "export const answer: number = 42;\n"
        );
        assert!(!snapshot.join("node_modules").exists());
        assert!(config.arguments.len() <= 8);

        std::fs::write(
            workspace.join("oversized.ts"),
            vec![b'x'; 2 * 1024 * 1024 + 1],
        )
        .expect("oversized source fixture");
        let degraded_task = daemon.create_task("Agent MCP degraded".into()).unwrap();
        let degraded = runtime
            .mcp_launch(degraded_task, &state_directory.join("agents/session-8"))
            .expect("MCP configured")
            .expect("GenUI-only MCP config");
        let degraded_arguments = degraded
            .arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(degraded_arguments.contains(&std::borrow::Cow::Borrowed("--enable-genui")));
        assert!(!degraded_arguments.contains(&std::borrow::Cow::Borrowed("--enable-deno-lsp")));
        assert!(
            !state_directory
                .join("brokered-mcp")
                .join(degraded_task.to_string())
                .join("workspace-snapshot")
                .exists()
        );
    }

    #[test]
    fn agent_session_capacity_is_reported_as_rate_limited() {
        assert_eq!(
            agent_start_error_response(StartError::Capacity).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
