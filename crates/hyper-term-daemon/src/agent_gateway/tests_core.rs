    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;

    use futures_util::StreamExt;
    use hyper_term_core::{ExecutionContextInputs, compile_execution_context};
    use hyper_term_protocol::{
        CollisionPolicy, EnvironmentPlan, ExecutionContextSpec, ExecutionMode,
        LocalMcpCredentialScope, LocalMcpServerLifecycle, RuntimeEnvironmentSpec,
        SandboxEnforcement, SandboxEnvironmentPolicy, SandboxFileSystemPolicy, SandboxLifetime,
        SandboxNetworkPolicy, SandboxProcessPolicy, SandboxProfile, SandboxResourceLimits,
        WorkspaceContextSpec,
    };
    use sha2::{Digest, Sha256};

    use super::test_support::{
        request, request_path, request_path_raw, wait_for_provider_readiness,
    };
    use super::*;

    fn approval_digest(daemon: &DaemonState, operation_id: OperationId) -> String {
        daemon
            .approval_detail(operation_id)
            .expect("approval detail")
            .detail_digest
            .to_string()
    }

    #[test]
    fn agent_diagnostics_drop_unsafe_controls_and_stay_bounded() {
        let diagnostic = format!("prefix\0{}suffix", "x".repeat(5000));
        let bounded = bounded_agent_diagnostic(&diagnostic);

        assert!(!bounded.contains('\0'));
        assert_eq!(bounded.chars().count(), 4096);
        assert!(bounded.starts_with("prefix"));
    }

    fn gateway_local_mcp_config(
        temporary: &tempfile::TempDir,
        workspace: &Path,
    ) -> LocalMcpServerConfig {
        let runtime_home = temporary.path().join("mcp-home");
        let runtime_temp = temporary.path().join("mcp-tmp");
        std::fs::create_dir_all(&runtime_home).unwrap();
        std::fs::create_dir_all(&runtime_temp).unwrap();
        let mut profile = SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy::default(),
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy::default(),
            platform: Default::default(),
            process: SandboxProcessPolicy::default(),
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneTask,
        };
        profile
            .process
            .allowed_executables
            .push(PathBuf::from("/bin/bash").canonicalize().unwrap());
        let context = ExecutionContextSpec {
            schema_version: hyper_term_protocol::EXECUTION_CONTEXT_SCHEMA_VERSION,
            context_id: "gateway:mcp:fixture:1".into(),
            context_revision: 1,
            mode: ExecutionMode::Hermetic,
            workspace: WorkspaceContextSpec {
                root: workspace.to_owned(),
                working_directory: workspace.to_owned(),
                runtime_home: runtime_home.canonicalize().unwrap(),
                runtime_temp: runtime_temp.canonicalize().unwrap(),
            },
            runtime: RuntimeEnvironmentSpec {
                path: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
                locale: "C.UTF-8".into(),
                timezone: "UTC".into(),
                terminal: "dumb".into(),
            },
            shell: None,
            environment: EnvironmentPlan {
                bindings: Vec::new(),
                collision_policy: CollisionPolicy::Deny,
            },
            credentials: Vec::new(),
            sandbox: Some(profile),
        };
        let (execution_context, _) =
            compile_execution_context(&context, &ExecutionContextInputs::default()).unwrap();
        let script = r#"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"gateway-fixture","version":"1.0.0"}}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"fixture.read","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}]}}' ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"gateway manager result"}],"structuredContent":{"text":"gateway manager result"},"isError":false}}' ;;
  esac
done
"#;
        let executable = PathBuf::from("/bin/sh").canonicalize().unwrap();
        LocalMcpServerConfig {
            server_id: "gateway_fixture".into(),
            executable_sha256: sha256_file(&executable).unwrap(),
            executable,
            arguments: [OsString::from("-c"), OsString::from(script)].into(),
            working_directory: workspace.to_owned(),
            execution_context,
            roots_snapshot_sha256: Some("a".repeat(64)),
            lifecycle: LocalMcpServerLifecycle::OneTask,
            credential_scope: LocalMcpCredentialScope::ServerLifetime,
        }
    }

    fn restart_history_gateway_config(
        workspace: &Path,
        state_directory: &Path,
        daemon: DaemonState,
        provider_home: &Path,
        executable: &Path,
    ) -> AgentGatewayConfig {
        AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: "0123456789abcdef0123456789abcdef".into(),
            workspace: workspace.to_owned(),
            state_directory: state_directory.to_owned(),
            daemon,
            provider_home: provider_home.to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: vec![AcpAgentProviderConfig {
                provider_id: "fixture-acp".into(),
                executable: executable.to_owned(),
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                implementation_version: "fixture-restart-1".into(),
            }],
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: provider_home.join("hyperd.sock"),
        }
    }

    #[test]
    fn provider_model_version_errors_become_actionable_status_text() {
        let nested = r#"ACP request 3 failed: {"code":-32603,"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-5.6-sol' model requires a newer version of Codex. Please upgrade to the latest app or CLI and try again.\"}}"}"#;
        assert_eq!(
            agent_error_summary(nested),
            "Model gpt-5.6-sol requires a newer Codex CLI · choose another model or update Codex"
        );
        assert!(!agent_error_summary(nested).contains("jsonrpc"));
        assert_eq!(
            agent_error_summary("Agent exited before the turn completed"),
            "Agent exited before the turn completed"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_session_drives_reviewed_local_mcp_over_real_stdio() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let codex = temporary.path().join("codex");
        std::fs::write(
            &codex,
            r#"#!/bin/sh
if [ "${1:-} ${2:-}" = "login status" ]; then exit 0; fi
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"userAgent":"mcp-gateway-fixture"}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"gpt-test","displayName":"GPT Test","description":"Fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Medium"}],"defaultReasoningEffort":"medium","isDefault":true}]}}' ;;
    *'"method":"skills/list"'*) printf '%s\n' '{"id":3,"result":{"data":[]}}' ;;
    *'"method":"thread/start"'*) printf '%s\n' '{"id":4,"result":{"thread":{"id":"mcp-gateway-thread"}}}' ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace: workspace.clone(),
            state_directory: temporary.path().join("gateway-state"),
            daemon: daemon.clone(),
            provider_home: temporary.path().to_owned(),
            codex_executable: Some(codex),
            codex_auth_file: None,
            acp_providers: Vec::new(),
            local_mcp_servers: vec![gateway_local_mcp_config(&temporary, &workspace)],
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: None,
            debug_capsule: None,
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let session_path = format!("/agent/session?token={token}&session_id=14&provider=codex");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"{}").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(session["task_id"].clone()).unwrap();
        let mcp_path = format!("/agent/session/mcp?token={token}&session_id=14");
        let (status, body) = request_path(gateway.address(), &mcp_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let inventory: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(inventory["registered"][0]["server_id"], "gateway_fixture");
        assert_eq!(inventory["active"].as_array().unwrap().len(), 0);

        let (status, body) = request_path(
            gateway.address(),
            &mcp_path,
            "POST",
            br#"{"server_id":"gateway_fixture"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        let launch: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(launch["state"], "waiting_human");
        let permission_path = format!("/agent/session/permission?token={token}&session_id=14");
        let approval = serde_json::json!({
            "operation_id": launch["operation_id"],
            "expected_revision": launch["operation_revision"],
            "approval_detail_digest": approval_digest(
                &daemon,
                serde_json::from_value(launch["operation_id"].clone()).unwrap(),
            ),
            "decision": "allow_once"
        });
        let (status, body) = request_path(
            gateway.address(),
            &permission_path,
            "POST",
            &serde_json::to_vec(&approval).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        let launched: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(launched["state"], "succeeded");
        assert_eq!(launched["runtime"]["server_name"], "gateway-fixture");
        assert_eq!(launched["runtime"]["tools"][0]["name"], "fixture.read");

        let call_path = format!("/agent/session/mcp/call?token={token}&session_id=14");
        let (status, body) = request_path(
            gateway.address(),
            &call_path,
            "POST",
            br#"{"server_id":"gateway_fixture","tool_name":"fixture.read","arguments":{"path":"README.md"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        let call: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let approval = serde_json::json!({
            "operation_id": call["operation_id"],
            "expected_revision": call["operation_revision"],
            "approval_detail_digest": approval_digest(
                &daemon,
                serde_json::from_value(call["operation_id"].clone()).unwrap(),
            ),
            "decision": "allow_once"
        });
        let (status, body) = request_path(
            gateway.address(),
            &permission_path,
            "POST",
            &serde_json::to_vec(&approval).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        let completed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(completed["state"], "succeeded");
        assert_eq!(completed["receipt"]["succeeded"], true);
        assert_eq!(
            completed["result"]["structuredContent"]["text"],
            "gateway manager result"
        );

        let (status, _) = request_path(gateway.address(), &session_path, "DELETE", b"").await;
        assert_eq!(status, StatusCode::NO_CONTENT.as_u16());
        assert!(
            gateway
                .runtime
                .local_mcp
                .active_server_receipts(task_id)
                .await
                .unwrap()
                .is_empty()
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn provider_endpoint_observes_login_without_gateway_restart() {
        let temporary = tempfile::tempdir().expect("temporary provider home");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let marker = temporary.path().join("authenticated");
        let codex = temporary.path().join("codex");
        let provider_fixture = r#"#!/bin/sh
if [ "${1:-} ${2:-}" = "login status" ]; then
  [ -f '__AUTH_MARKER__' ]
  exit
fi
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{"userAgent":"refresh-fixture"}}' ;;
    *'"method":"model/list"'*) printf '%s\n' '{"id":2,"result":{"data":[{"model":"gpt-test","displayName":"GPT Test","description":"Fixture","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Medium"}],"defaultReasoningEffort":"medium","isDefault":true}]}}' ;;
    *'"method":"skills/list"'*) printf '%s\n' '{"id":3,"result":{"data":[]}}' ;;
    *'"method":"thread/start"'*) printf '%s\n' '{"id":4,"result":{"thread":{"id":"refresh-thread"}}}' ;;
  esac
done
"#
        .replace("__AUTH_MARKER__", &marker.display().to_string());
        std::fs::write(&codex, provider_fixture).expect("provider fixture");
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700))
            .expect("provider fixture permissions");
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: DaemonState::open(temporary.path().join("daemon-state")).expect("daemon"),
            provider_home: temporary.path().to_owned(),
            codex_executable: Some(codex),
            codex_auth_file: Some(temporary.path().join(".codex/auth.json")),
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
        .expect("gateway");
        let path = format!("/agent/providers?token={token}");
        wait_for_provider_readiness(gateway.address(), &path, "login_required").await;
        let session_path = format!("/agent/session?token={token}&session_id=1&provider=codex");
        assert_eq!(
            request_path(gateway.address(), &session_path, "POST", b"")
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE.as_u16()
        );

        std::fs::write(&marker, "ready").expect("complete login");
        wait_for_provider_readiness(gateway.address(), &path, "authenticated").await;
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16(), "{body:?}");
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/attention?token=wrong",
                "GET",
                b""
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let attention_path = format!("/agent/attention?token={token}");
        let (status, body) = request_path(gateway.address(), &attention_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let attention: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(attention["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(attention["sessions"][0]["session_id"], 1);
        assert_eq!(attention["sessions"][0]["provider"], "codex");
        assert_eq!(attention["sessions"][0]["status"], "ready");
        assert!(
            attention["sessions"][0]["document_revision"]
                .as_u64()
                .is_some_and(|revision| revision > 0)
        );
        assert!(attention["sessions"][0].get("error").is_none());
        gateway.shutdown().await.expect("shutdown");
    }

    #[cfg(target_os = "macos")]
    fn run_git(cwd: &Path, arguments: &[&str]) {
        let output = std::process::Command::new("/usr/bin/git")
            .arg("-C")
            .arg(cwd)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    fn fake_lima_runner(root: &Path) -> hyper_term_sandbox::LimaTaskRunner {
        let executable = root.join("limactl");
        let environment_marker = root.join("limactl-environment");
        let script = format!(
            "#!/bin/sh\nset -eu\nif [ \"${{1:-}}\" = \"--version\" ]; then echo 'limactl version 2.1.1'; exit 0; fi\naction=''\nlast=''\nfor argument in \"$@\"; do\n  last=\"$argument\"\n  case \"$argument\" in validate|start|shell|stop|delete) [ -n \"$action\" ] || action=\"$argument\";; esac\ndone\nif [ \"$action\" = start ]; then printf '%s\\n' \"${{last%/*}}\" > '{}'; fi\nif [ \"$action\" = shell ]; then environment=$(cat '{}'); printf '\\377\\000\\001' > \"$environment/worktree/data.bin\"; printf 'from tier2\\n' > \"$environment/worktree/generated.txt\"; printf 'tier2-output\\n'; fi\n",
            environment_marker.display(),
            environment_marker.display()
        );
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let image = root.join("image.qcow2");
        std::fs::write(&image, b"local pinned image").unwrap();
        hyper_term_sandbox::LimaTaskRunner::with_executable(
            executable,
            hyper_term_sandbox::LimaRunnerConfig {
                image: hyper_term_sandbox::LimaImage {
                    path: image,
                    sha256: Sha256::digest(b"local pinned image")
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    arch: "aarch64".into(),
                },
                vm_type: "vz".into(),
                cpus: 2,
                memory_mib: 1_024,
                disk_gib: 4,
                start_timeout: Duration::from_secs(2),
                task_timeout: Duration::from_secs(2),
                max_output_bytes: 64 * 1024,
            },
        )
        .unwrap()
    }

    fn draft_fixture() -> crate::artifact_store::StoredGenUiArtifact {
        crate::artifact_store::StoredGenUiArtifact {
            metadata: hyper_term_protocol::AcceptedGenUiArtifact {
                artifact_id: ArtifactId::new(),
                source_revision: 7,
                entrypoint: "/App.tsx".into(),
                content_digest: "a".repeat(64),
                compiler: hyper_term_protocol::GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
            },
            source_files: BTreeMap::from([
                ("/App.tsx".into(), "export default () => null;".into()),
                ("/theme.ts".into(), "export const accent = 'green';".into()),
            ]),
            bundle: "globalThis.fixture=true;".into(),
            css: String::new(),
            source_map: "{}".into(),
        }
    }

    #[test]
    fn acp_terminal_output_retains_the_tail_at_a_utf8_boundary() {
        assert_eq!(
            retain_terminal_output("stdout", "stderr", 10),
            ("doutstderr".into(), true)
        );
        assert_eq!(retain_terminal_output("ab🙂cd", "", 5), ("cd".into(), true));
        assert_eq!(retain_terminal_output("ok", "", 2), ("ok".into(), false));
    }

    fn capsule_fixture() -> GenUiBugCapsule {
        let artifact = draft_fixture();
        let editor = ArtifactEditorCheckpoint {
            schema_version: 1,
            artifact_id: artifact.metadata.artifact_id,
            base_source_revision: artifact.metadata.source_revision,
            revision: 0,
            state_digest: "b".repeat(64),
            entrypoint: artifact.metadata.entrypoint.clone(),
            files: artifact.source_files.clone(),
            active_path: artifact.metadata.entrypoint.clone(),
            view: crate::artifact_editor_store::ArtifactEditorView::Trace,
            selections: BTreeMap::new(),
        };
        let runtime = GenUiRuntimeTraceProjection {
            artifact_id: artifact.metadata.artifact_id,
            source_revision: artifact.metadata.source_revision,
            projection_digest: "c".repeat(64),
            events: Vec::new(),
        };
        build_bug_capsule(
            &artifact,
            &editor,
            &runtime,
            GenUiBugCapsuleEnvironment {
                hyper_term_version: "0.1.0".into(),
                os: "macos".into(),
                architecture: "aarch64".into(),
                deno_runtime_version: None,
                deno_executable_digest: None,
                compiler_script_digest: None,
                compiler_wasm_digest: None,
            },
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn acp_containment_reads_only_the_adapter_provider_and_auth_roots() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("runtime");
        let adapter_root = runtime.join("acp/node_modules");
        let adapter = adapter_root.join("@agentclientprotocol/codex-acp/dist/index.js");
        let provider_root = temporary.path().join("provider/node_modules");
        let provider = provider_root.join("@openai/codex/bin/codex.js");
        let executable = runtime.join("deno");
        let bin = temporary.path().join("bin");
        let node_root = temporary.path().join("Cellar/node/26.0.0");
        let node = node_root.join("bin/node");
        let home = temporary.path().join("home");
        let codex_root = home.join(".codex");
        let auth = codex_root.join("auth.json");
        let unrelated = home.join("Documents/private.txt");
        for directory in [
            adapter.parent().unwrap(),
            provider.parent().unwrap(),
            &bin,
            node.parent().unwrap(),
            &codex_root,
            unrelated.parent().unwrap(),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for file in [&adapter, &provider, &auth, &unrelated] {
            std::fs::write(file, "fixture").unwrap();
        }
        std::fs::write(&executable, "#!/usr/bin/env node\n").unwrap();
        std::fs::write(&node, "node fixture").unwrap();
        symlink(&node, bin.join("node")).unwrap();
        let provider = AcpAgentProviderConfig {
            provider_id: "codex-acp".into(),
            executable,
            arguments: vec!["run".into(), adapter.into_os_string()],
            environment: BTreeMap::from([
                ("HOME".into(), home.clone().into_os_string()),
                ("PATH".into(), bin.into_os_string()),
                ("CODEX_PATH".into(), provider.into_os_string()),
            ]),
            implementation_version: "fixture-1".into(),
        };

        let paths = acp_provider_read_paths(&provider);
        assert!(paths.contains(&adapter_root.canonicalize().unwrap()));
        assert!(paths.contains(&provider_root.canonicalize().unwrap()));
        assert!(paths.contains(&node_root.canonicalize().unwrap()));
        assert!(paths.contains(&auth.canonicalize().unwrap()));
        assert!(!paths.contains(&codex_root.canonicalize().unwrap()));
        assert!(!paths.contains(&home.canonicalize().unwrap()));
        assert!(!paths.contains(&unrelated.canonicalize().unwrap()));
        assert_eq!(
            acp_network_allowed_hosts("codex-acp").unwrap(),
            CODEX_NETWORK_ALLOWED_HOSTS
        );
    }

    #[test]
    fn claude_containment_reads_preferences_but_not_host_credentials() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = temporary.path().join("runtime");
        let executable = runtime.join("deno");
        let adapter = runtime.join("acp/claude-agent-acp.js");
        let provider_root = temporary.path().join("provider");
        let provider_executable = provider_root.join("claude");
        let home = temporary.path().join("home");
        let claude_home = home.join(".claude");
        let settings = claude_home.join("settings.json");
        let skills = claude_home.join("skills");
        let credentials = claude_home.join(".credentials.json");
        let keychains = home.join("Library/Keychains");
        for directory in [
            &runtime,
            adapter.parent().unwrap(),
            &provider_root,
            &skills,
            &keychains,
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for file in [
            &executable,
            &adapter,
            &provider_executable,
            &settings,
            &credentials,
        ] {
            std::fs::write(file, "fixture").unwrap();
        }
        let provider = AcpAgentProviderConfig {
            provider_id: "claude-acp".into(),
            executable,
            arguments: vec!["run".into(), adapter.into_os_string()],
            environment: BTreeMap::from([
                ("HOME".into(), home.into_os_string()),
                ("PATH".into(), provider_root.into_os_string()),
                (
                    "CLAUDE_CODE_EXECUTABLE".into(),
                    provider_executable.into_os_string(),
                ),
            ]),
            implementation_version: "fixture-1".into(),
        };

        let paths = acp_provider_read_paths(&provider);
        assert!(paths.contains(&settings.canonicalize().unwrap()));
        assert!(paths.contains(&skills.canonicalize().unwrap()));
        assert!(!paths.contains(&claude_home.canonicalize().unwrap()));
        assert!(!paths.contains(&credentials.canonicalize().unwrap()));
        assert!(!paths.contains(&keychains.canonicalize().unwrap()));
    }

    #[test]
    fn daemon_restart_reconciles_a_durable_workspace_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let daemon_state = temporary.path().join("daemon-state");
        let gateway_state = temporary.path().join("gateway-state");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&gateway_state).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("App.tsx"), "before\n").unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let daemon = DaemonState::open(&daemon_state).unwrap();
        let (task_id, operation_id, dispatching) = workspace_dispatch(&daemon);
        let set =
            prepare_workspace_apply_set(&workspace, vec![("App.tsx".into(), "after\n".into())])
                .unwrap();
        let result = apply_workspace_set_plan_durable(
            &workspace,
            &gateway_state,
            WorkspaceTransactionContext {
                task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &set,
        )
        .unwrap();
        assert!(matches!(result, DurableWorkspaceApplyResult::Committed(_)));
        drop(daemon);

        let daemon = DaemonState::open(&daemon_state).unwrap();
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            OperationState::UnknownExecution
        );
        let recovery = recover_workspace_transactions(&workspace, &gateway_state).unwrap();
        assert!(reconcile_workspace_recovery(&daemon, &gateway_state, recovery).is_none());
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            OperationState::Succeeded
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("App.tsx")).unwrap(),
            "after\n"
        );
        assert!(
            std::fs::read_dir(gateway_state.join("workspace-transactions"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn daemon_restart_reconciles_a_safely_rolled_back_workspace_apply() {
        let temporary = tempfile::tempdir().unwrap();
        let daemon_state = temporary.path().join("daemon-state");
        let gateway_state = temporary.path().join("gateway-state");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&gateway_state).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("one.ts"), "one before\n").unwrap();
        std::fs::write(workspace.join("two.ts"), "two before\n").unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let daemon = DaemonState::open(&daemon_state).unwrap();
        let (task_id, operation_id, dispatching) = workspace_dispatch(&daemon);
        let set = prepare_workspace_apply_set(
            &workspace,
            vec![
                ("one.ts".into(), "one after\n".into()),
                ("two.ts".into(), "two after\n".into()),
            ],
        )
        .unwrap();
        std::fs::write(workspace.join("two.ts"), "external writer\n").unwrap();
        let result = apply_workspace_set_plan_durable(
            &workspace,
            &gateway_state,
            WorkspaceTransactionContext {
                task_id,
                operation_id,
                operation_revision: dispatching.revision,
            },
            &set,
        )
        .unwrap();
        assert!(matches!(result, DurableWorkspaceApplyResult::RolledBack(_)));
        drop(daemon);

        let daemon = DaemonState::open(&daemon_state).unwrap();
        let recovery = recover_workspace_transactions(&workspace, &gateway_state).unwrap();
        assert!(reconcile_workspace_recovery(&daemon, &gateway_state, recovery).is_none());
        assert_eq!(
            daemon.operation(operation_id).unwrap().state,
            OperationState::Failed
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("one.ts")).unwrap(),
            "one before\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("two.ts")).unwrap(),
            "external writer\n"
        );
    }

    fn workspace_dispatch(
        daemon: &DaemonState,
    ) -> (TaskId, OperationId, hyper_term_core::OperationRecord) {
        let task_id = daemon.create_task("workspace recovery".into()).unwrap();
        let proposed = daemon
            .propose_operation(
                task_id,
                OperationKind::FileEdit,
                OperationAction::Opaque {
                    kind: "hyper_term.workspace.apply".into(),
                    payload_digest: "a".repeat(64),
                },
                "apply artifact".into(),
                RiskClass::WorkspaceWrite,
                vec!["workspace_write".into()],
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
        (task_id, proposed.operation_id, dispatching)
    }

    #[test]
    fn artifact_drafts_require_the_current_revision_and_rust_owned_file_set() {
        let artifact = draft_fixture();
        let changed = BTreeMap::from([
            (
                "/App.tsx".into(),
                "export default () => <main>Live</main>;".into(),
            ),
            ("/theme.ts".into(), "export const accent = 'green';".into()),
        ]);
        let request = validate_artifact_draft(
            &artifact,
            AgentArtifactDraftRequest {
                base_source_revision: 7,
                entrypoint: "/App.tsx".into(),
                files: changed.clone(),
            },
        )
        .unwrap();
        assert_eq!(request.source_revision, 8);
        assert_eq!(request.files, changed);
        assert!(matches!(
            validate_artifact_draft(
                &artifact,
                AgentArtifactDraftRequest {
                    base_source_revision: 6,
                    entrypoint: "/App.tsx".into(),
                    files: request.files.clone(),
                }
            ),
            Err(ArtifactDraftError::StaleRevision)
        ));
        let mut escaped = request.files;
        escaped.insert("/invented.ts".into(), "export {};".into());
        assert!(matches!(
            validate_artifact_draft(
                &artifact,
                AgentArtifactDraftRequest {
                    base_source_revision: 7,
                    entrypoint: "/App.tsx".into(),
                    files: escaped,
                }
            ),
            Err(ArtifactDraftError::InvalidRequest)
        ));
        assert!(matches!(
            validate_artifact_draft(
                &artifact,
                AgentArtifactDraftRequest {
                    base_source_revision: 7,
                    entrypoint: "/App.tsx".into(),
                    files: artifact.source_files.clone(),
                }
            ),
            Err(ArtifactDraftError::NoChanges)
        ));
    }

    #[test]
    fn base_fenced_acceptance_rejects_an_artifact_replaced_during_build() {
        let temporary = tempfile::tempdir().unwrap();
        let daemon_state = temporary.path().join("daemon-state");
        let daemon = DaemonState::open(&daemon_state).unwrap();
        let task_id = daemon.create_task("artifact base fence".into()).unwrap();
        let dispatch = || {
            let proposed = daemon
                .propose_operation(
                    task_id,
                    OperationKind::McpTool,
                    OperationAction::Opaque {
                        kind: "hyper_term.genui.compile".into(),
                        payload_digest: "a".repeat(64),
                    },
                    "compile artifact".into(),
                    RiskClass::ReadOnly,
                    vec!["artifact_build".into()],
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
            daemon
                .begin_operation(task_id, proposed.operation_id, authorized.revision)
                .unwrap()
        };
        let candidate = |revision: u64, label: &str| {
            let bundle = format!("globalThis.label={label:?};");
            GenUiArtifactCandidate {
                schema_version: 1,
                source_revision: revision,
                entrypoint: "/App.tsx".into(),
                source_files: BTreeMap::from([(
                    "/App.tsx".into(),
                    format!("export default () => {label:?};"),
                )]),
                content_digest: sha256_bytes(bundle.as_bytes()),
                bundle,
                css: String::new(),
                source_map: "{}".into(),
                compiler: hyper_term_protocol::GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
                diagnostics: Vec::new(),
            }
        };
        let first_operation = dispatch();
        let first = daemon
            .accept_genui_artifact(
                task_id,
                first_operation.operation_id,
                first_operation.revision,
                candidate(1, "first"),
            )
            .unwrap();
        let second_operation = dispatch();
        let second = daemon
            .accept_genui_artifact_from_base(
                task_id,
                second_operation.operation_id,
                second_operation.revision,
                first.artifact_id,
                first.source_revision,
                candidate(2, "second"),
            )
            .unwrap();
        let stale_operation = dispatch();
        assert!(matches!(
            daemon.accept_genui_artifact_from_base(
                task_id,
                stale_operation.operation_id,
                stale_operation.revision,
                first.artifact_id,
                first.source_revision,
                candidate(2, "stale"),
            ),
            Err(crate::DaemonError::ArtifactBaseNotCurrent { .. })
        ));
        assert_eq!(
            daemon.active_genui_artifact(task_id).unwrap().unwrap(),
            second
        );
        let history = daemon
            .genui_artifact_history(task_id, second.artifact_id)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].artifact, second);
        assert_eq!(history[1].artifact, first);
        assert!(history[0].event_sequence > history[1].event_sequence);
        assert_eq!(
            daemon
                .read_genui_artifact_revision(task_id, second.artifact_id, first.artifact_id)
                .unwrap()
                .source_files["/App.tsx"],
            "export default () => \"first\";"
        );

        drop(daemon);
        let reopened = DaemonState::open(&daemon_state).unwrap();
        let reopened_history = reopened
            .genui_artifact_history(task_id, second.artifact_id)
            .unwrap();
        assert_eq!(reopened_history, history);
        assert_eq!(
            reopened
                .read_genui_artifact_revision(task_id, second.artifact_id, first.artifact_id)
                .unwrap()
                .metadata,
            first
        );
    }

    #[test]
    fn preview_bootstrap_is_inline_escaped_and_keeps_the_runtime_capsule() {
        let artifact_id = ArtifactId::new();
        let stored = crate::artifact_store::StoredGenUiArtifact {
            metadata: hyper_term_protocol::AcceptedGenUiArtifact {
                artifact_id,
                source_revision: 3,
                entrypoint: "/App.tsx".into(),
                content_digest: "a".repeat(64),
                compiler: hyper_term_protocol::GenUiCompilerIdentity {
                    name: "esbuild-wasm".into(),
                    version: "0.28.1".into(),
                },
            },
            source_files: BTreeMap::from([(
                "/App.tsx".into(),
                "export default () => null;".into(),
            )]),
            bundle: "globalThis.value='</script><script>bad()'".into(),
            css: "main::after{content:'<&>'}".into(),
            source_map: "{}".into(),
        };
        let shell = format!(
            "<html><head>{ARTIFACT_BOOTSTRAP_MARKER}<script>hyper_term_preview_boot</script></head></html>"
        );
        let document = render_preview_document(&shell, &stored).unwrap();
        assert!(!document.contains("</script><script>bad()"));
        assert!(document.contains("\\u003c/script\\u003e\\u003cscript\\u003ebad()"));
        assert!(document.contains(&artifact_id.to_string()));
        assert!(document.contains("hyper_term_preview_boot"));
        assert!(document.contains("\"source_map\":\"{}\""));
        assert!(!document.contains(ARTIFACT_BOOTSTRAP_MARKER));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_capsule_endpoint_requires_only_the_desktop_gateway_token() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workbench_assets = temporary.path().join("workbench");
        std::fs::create_dir_all(&workbench_assets).unwrap();
        std::fs::write(
            workbench_assets.join("index.html"),
            "<html><body>offline-capsule-workbench</body></html>",
        )
        .unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let expected = capsule_fixture();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace,
            state_directory: temporary.path().join("gateway-state"),
            daemon: DaemonState::open(temporary.path().join("daemon-state")).unwrap(),
            provider_home: temporary.path().to_owned(),
            codex_executable: None,
            codex_auth_file: None,
            acp_providers: Vec::new(),
            local_mcp_servers: Vec::new(),
            mcp_executable: None,
            genui_runtime: None,
            workbench_assets: Some(workbench_assets),
            debug_capsule: Some(expected.clone()),
            tier2_runner: None,
            control_socket: temporary.path().join("hyperd.sock"),
        })
        .await
        .unwrap();
        let path = format!("/agent/debug-capsule?token={token}");
        let (status, body) = request_path(gateway.address(), &path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let actual: GenUiBugCapsule = serde_json::from_slice(&body).unwrap();
        assert_eq!(actual, expected);
        let workbench_path = format!("/agent/workbench/?surface=capsule&token={token}");
        let (status, body) = request_path(gateway.address(), &workbench_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("offline-capsule-workbench")
        );
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/debug-capsule?token=wrong",
                "GET",
                b""
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        assert_eq!(
            request_path(
                gateway.address(),
                "/agent/workbench/?surface=capsule&token=wrong",
                "GET",
                b""
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        gateway.shutdown().await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    async fn tier2_review_endpoint_requires_diff_approval_before_workspace_apply() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        run_git(&workspace, &["init", "-q"]);
        run_git(&workspace, &["config", "user.name", "Hyper Term Test"]);
        run_git(
            &workspace,
            &["config", "user.email", "hyper-term@example.invalid"],
        );
        std::fs::write(workspace.join("README.md"), "source\n").unwrap();
        run_git(&workspace, &["add", "."]);
        run_git(&workspace, &["commit", "-qm", "fixture"]);
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).unwrap();
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"model/list\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[{\"model\":\"gpt-test\",\"displayName\":\"GPT Test\",\"description\":\"Fixture\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"medium\",\"description\":\"Medium\"}],\"defaultReasoningEffort\":\"medium\",\"isDefault\":true}]}}' ;;\n    *'\"method\":\"skills/list\"'*) printf '%s\\n' '{\"id\":3,\"result\":{\"data\":[]}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":4,\"result\":{\"thread\":{\"id\":\"tier2-thread\"}}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_codex, std::fs::Permissions::from_mode(0o700)).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_owned();
        let gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: token.clone(),
            workspace: workspace.clone(),
            state_directory: temporary.path().join("gateway-state"),
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
        .unwrap();
        let session_path = format!("/agent/session?token={token}&session_id=6");
        let (status, body) = request_path(gateway.address(), &session_path, "POST", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let task_id: TaskId = serde_json::from_value(session["task_id"].clone()).unwrap();
        let operation = daemon
            .propose_operation(
                task_id,
                OperationKind::Shell,
                OperationAction::Shell {
                    command: hyper_term_protocol::TerminalCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "printf generated > generated.txt".into()],
                        cwd: Some(workspace.clone()),
                        env: BTreeMap::new(),
                    },
                },
                "run an isolated code task".into(),
                RiskClass::WorkspaceWrite,
                vec!["shell".into(), "sandbox.isolated_task".into()],
            )
            .unwrap();
        let authorized = daemon
            .decide_permission(
                task_id,
                operation.operation_id,
                operation.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        daemon
            .dispatch_isolated_task(
                task_id,
                operation.operation_id,
                authorized.revision,
                &fake_lima_runner(temporary.path()),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
        assert!(!workspace.join("generated.txt").exists());

        let tier2_path = format!("/agent/session/tier2?token={token}&session_id=6");
        let (status, body) = request_path(gateway.address(), &tier2_path, "GET", b"").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let results: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            results["results"][0]["changed_files"][0]["path"],
            "data.bin"
        );
        assert!(results["results"][0].get("acceptance").is_none());

        let source_body = serde_json::to_vec(&serde_json::json!({
            "source_operation_id": operation.operation_id,
        }))
        .unwrap();
        let preview_path = format!("/agent/session/tier2/preview?token={token}&session_id=6");
        let (status, body) =
            request_path(gateway.address(), &preview_path, "POST", &source_body).await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let preview: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(preview["changes"][0]["target_path"], "data.bin");
        assert_eq!(preview["changes"][0]["binary"], true);
        assert_eq!(preview["changes"][0]["base_bytes"], 0);
        assert_eq!(preview["changes"][0]["proposed_bytes"], 3);
        assert_eq!(preview["changes"][0]["hunks"], serde_json::json!([]));
        assert_eq!(preview["changes"][1]["target_path"], "generated.txt");
        assert!(
            preview["changes"][1]["hunks"][0]["patch"]
                .as_str()
                .unwrap()
                .contains("from tier2")
        );
        let (_, body) = request_path(gateway.address(), &tier2_path, "GET", b"").await;
        assert!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["results"][0]
                .get("acceptance")
                .is_none(),
            "opening a Diff must not create an approval"
        );
        let review_path = format!("/agent/session/tier2/review?token={token}&session_id=6");
        let (status, body) =
            request_path(gateway.address(), &review_path, "POST", &source_body).await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16());
        let review: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(review["state"], "waiting_human");
        assert_eq!(review["changed_file_count"], 2);

        let discard_path = format!("/agent/session/tier2/discard?token={token}&session_id=6");
        assert_eq!(
            request_path(gateway.address(), &discard_path, "POST", &source_body)
                .await
                .0,
            StatusCode::CONFLICT.as_u16()
        );
        let permission = serde_json::to_vec(&serde_json::json!({
            "operation_id": review["operation_id"],
            "expected_revision": review["operation_revision"],
            "approval_detail_digest": approval_digest(
                &daemon,
                serde_json::from_value(review["operation_id"].clone()).unwrap(),
            ),
            "decision": "allow_once",
        }))
        .unwrap();
        let permission_path = format!("/agent/session/permission?token={token}&session_id=6");
        assert_eq!(
            request_path(gateway.address(), &permission_path, "POST", &permission)
                .await
                .0,
            StatusCode::ACCEPTED.as_u16()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("generated.txt")).unwrap(),
            "from tier2\n"
        );
        let (_, body) = request_path(gateway.address(), &tier2_path, "GET", b"").await;
        assert!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["results"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_session_streams_a_turn_into_the_block_document() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("gateway-state");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let daemon = DaemonState::open(temporary.path().join("daemon-state")).expect("daemon");
        let fake_codex = temporary.path().join("codex");
        std::fs::write(
            &fake_codex,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"id\":1,\"result\":{\"userAgent\":\"fake-codex\"}}' ;;\n    *'\"method\":\"model/list\"'*) printf '%s\\n' '{\"id\":2,\"result\":{\"data\":[{\"model\":\"gpt-test\",\"displayName\":\"GPT Test\",\"description\":\"Fixture\",\"hidden\":false,\"supportedReasoningEfforts\":[{\"reasoningEffort\":\"medium\",\"description\":\"Medium\"}],\"defaultReasoningEffort\":\"medium\",\"isDefault\":true}]}}' ;;\n    *'\"method\":\"skills/list\"'*) printf '%s\\n' '{\"id\":3,\"result\":{\"data\":[]}}' ;;\n    *'\"method\":\"thread/start\"'*) printf '%s\\n' '{\"id\":4,\"result\":{\"thread\":{\"id\":\"thread-3\"}}}' ;;\n    *'\"method\":\"thread/goal/set\"'*) printf '%s\\n' '{\"id\":5,\"result\":{\"goal\":{\"threadId\":\"thread-3\",\"objective\":\"Ship the compact Agent UI\",\"status\":\"active\",\"tokenBudget\":50000,\"tokensUsed\":1200,\"timeUsedSeconds\":90,\"createdAt\":1,\"updatedAt\":2}}}' ;;\n    *'\"method\":\"turn/start\"'*)\n      printf '%s\\n' '{\"id\":6,\"result\":{\"turn\":{\"id\":\"turn-1\"}}}'\n      printf '%s\\n' '{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread-3\",\"turnId\":\"turn-1\",\"itemId\":\"message-1\",\"delta\":\"Hyper Term \"}}'\n      printf '%s\\n' '{\"method\":\"item/agentMessage/delta\",\"params\":{\"threadId\":\"thread-3\",\"turnId\":\"turn-1\",\"itemId\":\"message-1\",\"delta\":\"Agent is live.\"}}'\n      printf '%s\\n' '{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"thread-3\",\"turn\":{\"id\":\"turn-1\",\"status\":\"completed\"}}}' ;;\n  esac\ndone\n",
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

        assert_eq!(
            request(gateway.address(), "wrong-token", 3, "POST").await.0,
            StatusCode::UNAUTHORIZED.as_u16()
        );
        let (status, body) = request(gateway.address(), &token, 3, "POST").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let response: serde_json::Value = serde_json::from_slice(&body).expect("start response");
        assert_eq!(response["provider"], "codex");
        assert_eq!(response["protocol"], "codex-app-server-v2");
        assert_eq!(response["status"], "ready");
        assert_eq!(response["thread_id"], "thread-3");

        let turn_path = format!("/agent/session/turn?token={token}&session_id=3");
        let (status, body) = request_path(
            gateway.address(),
            &turn_path,
            "POST",
            b"/goal Ship the compact Agent UI",
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");
        let (_, body) = request(gateway.address(), &token, 3, "GET").await;
        let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot["goal"]["objective"], "Ship the compact Agent UI");
        assert_eq!(snapshot["goal"]["status"], "active");
        assert_eq!(snapshot["goal"]["time_used_seconds"], 90);

        let stream_response = stream_session(
            State(gateway.runtime.clone()),
            Query(AgentSessionQuery {
                token: Some(token.clone()),
                session_id: Some(3),
                provider: None,
            }),
        )
        .await;
        assert_eq!(stream_response.status(), StatusCode::OK);
        assert_eq!(
            stream_response.headers()[CONTENT_TYPE],
            "application/x-ndjson; charset=utf-8"
        );
        assert_eq!(stream_response.headers()[CACHE_CONTROL], "no-store");
        let mut updates = stream_response.into_body().into_data_stream();
        let initial = tokio::time::timeout(Duration::from_secs(1), updates.next())
            .await
            .expect("initial stream timeout")
            .expect("initial stream frame")
            .expect("initial stream body");
        let initial: serde_json::Value =
            serde_json::from_slice(initial.as_ref()).expect("initial NDJSON state");
        assert_eq!(initial["type"], "state");
        assert_eq!(initial["status"], "ready");
        assert_eq!(initial["goal"]["objective"], "Ship the compact Agent UI");
        assert!(initial["document_revision"].as_u64().is_some());
        assert!(initial.get("document").is_none());

        // A brokered MCP server proposes directly through the Rust authority;
        // there is intentionally no matching ACP pending_effect in this
        // process. The Agent snapshot and stream must still expose the live
        // approval instead of rendering its approval block as archived.
        let task_id: TaskId = serde_json::from_value(response["task_id"].clone()).unwrap();
        let brokered = daemon
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Compile a bounded GenUI artifact".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let (_, body) = request(gateway.address(), &token, 3, "GET").await;
        let waiting_snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(waiting_snapshot["status"], "waiting_approval");
        assert_eq!(
            waiting_snapshot["pending_operation_id"],
            brokered.operation_id.to_string()
        );
        loop {
            let body = tokio::time::timeout(Duration::from_secs(1), updates.next())
                .await
                .expect("brokered MCP stream update timeout")
                .expect("Agent stream stayed open")
                .expect("Agent stream body");
            let frame: serde_json::Value = serde_json::from_slice(body.as_ref()).unwrap();
            if frame["type"] == "state" && frame["status"] == "waiting_approval" {
                assert_eq!(
                    frame["pending_operation_id"],
                    brokered.operation_id.to_string()
                );
                break;
            }
        }
        let permission_path = format!("/agent/session/permission?token={token}&session_id=3");
        let permission = serde_json::to_vec(&serde_json::json!({
            "operation_id": brokered.operation_id,
            "expected_revision": brokered.revision,
            "approval_detail_digest": approval_digest(&daemon, brokered.operation_id),
            "decision": "reject_once",
        }))
        .unwrap();
        assert_eq!(
            request_path(gateway.address(), &permission_path, "POST", &permission)
                .await
                .0,
            StatusCode::ACCEPTED.as_u16()
        );
        loop {
            let body = tokio::time::timeout(Duration::from_secs(1), updates.next())
                .await
                .expect("brokered MCP decision stream timeout")
                .expect("Agent stream stayed open")
                .expect("Agent stream body");
            let frame: serde_json::Value = serde_json::from_slice(body.as_ref()).unwrap();
            if frame["type"] == "state"
                && frame["status"] == "ready"
                && frame["pending_operation_id"].is_null()
            {
                break;
            }
        }

        let unrelated_task = gateway
            .runtime
            .config
            .daemon
            .create_task("unrelated stream task".into())
            .expect("create unrelated task");
        gateway
            .runtime
            .config
            .daemon
            .append_message(
                unrelated_task,
                BlockId::new(),
                MessageRole::Agent,
                None,
                "must not cross the session boundary".into(),
            )
            .expect("append unrelated message");
        let deadline = tokio::time::Instant::now() + Duration::from_millis(150);
        while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, updates.next()).await {
            assert!(
                !String::from_utf8_lossy(&frame).contains("must not cross the session boundary"),
                "Agent stream leaked another task's block patch"
            );
        }

        let (status, body) = request_path(
            gateway.address(),
            &turn_path,
            "POST",
            b"Reply with the live marker",
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED.as_u16(), "{body:?}");

        let mut saw_patch = false;
        loop {
            let body = tokio::time::timeout(Duration::from_secs(2), updates.next())
                .await
                .expect("Agent stream update timeout")
                .expect("Agent stream stayed open")
                .expect("Agent stream body");
            let frame: serde_json::Value =
                serde_json::from_slice(body.as_ref()).expect("NDJSON Agent stream frame");
            match frame["type"].as_str() {
                Some("patch") => {
                    saw_patch = true;
                    assert!(frame["patch"]["target_revision"].as_u64().is_some());
                    assert!(frame.get("document").is_none());
                }
                Some("state") if frame["status"] == "completed" => break,
                Some("state" | "resync") => {}
                other => panic!("unexpected Agent stream frame: {other:?}"),
            }
        }
        assert!(saw_patch, "Agent turn should emit canonical block patches");
        let (status, body) = request(gateway.address(), &token, 3, "GET").await;
        assert_eq!(status, StatusCode::OK.as_u16());
        let snapshot: serde_json::Value =
            serde_json::from_slice(&body).expect("final snapshot response");
        let blocks = snapshot["document"]["blocks"]
            .as_array()
            .expect("snapshot blocks");
        assert!(blocks.iter().any(|block| {
            block["payload"]["role"] == "user"
                && block["payload"]["text"] == "Reply with the live marker"
        }));
        assert!(blocks.iter().any(|block| {
            block["payload"]["role"] == "agent"
                && block["payload"]["text"] == "Hyper Term Agent is live."
        }));

        assert_eq!(
            request(gateway.address(), &token, 3, "DELETE").await.0,
            StatusCode::NO_CONTENT.as_u16()
        );
        gateway.shutdown().await.expect("shutdown gateway");
    }
