#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use hyper_term_core::{TerminalEvent, TerminalReplay};
use hyper_term_daemon::{BrokeredMcpRuntimeConfig, DaemonError, DaemonState, spawn_unix_server};
use hyper_term_protocol::{
    ApprovalActionDetail, ApprovalDetailDigest, BlockPayload, ClientId, ContextDigest,
    ControlRequest, ControlRequestEnvelope, ControlResponse, DomainEvent, EventEnvelope,
    GenUiArtifactCandidate, GenUiCompilerIdentity, LocalMcpCredentialScope, LocalMcpServerLaunch,
    LocalMcpServerLifecycle, LocalMcpServerRuntimeReceipt, LocalMcpToolCall,
    LocalMcpToolCallReceipt, LocalMcpToolContractReceipt, McpArgumentsDigest,
    McpCapabilitiesDigest, McpCatalogDigest, McpRuntimeIdentityDigest, McpToolContractDigest,
    McpToolResultDigest, OperationAction, OperationCompletion, OperationKind, OperationOutcome,
    OperationState, PermissionDecision, RequestId, RiskClass, SandboxProfileDigest,
    TerminalCommand, TerminalDataFrame, TerminalInputFrame, TerminalSize, WireFrame,
    canonical_mcp_json_bytes, read_frame, write_frame,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn json_sha256(value: &impl Serialize) -> String {
    Sha256::digest(serde_json::to_vec(value).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn mcp_json_sha256(value: &serde_json::Value) -> String {
    Sha256::digest(canonical_mcp_json_bytes(value).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Serialize)]
struct ToolContractIdentity<'a> {
    planned_runtime_identity: &'a str,
    name: &'a str,
    input_schema_sha256: &'a str,
    output_schema_sha256: Option<&'a str>,
}

#[derive(Serialize)]
struct RuntimeIdentity<'a> {
    planned_runtime_identity: &'a str,
    negotiated_protocol_version: &'a str,
    server_name: &'a str,
    server_version: &'a str,
    enforced_sandbox_profile_digest: &'a str,
    capabilities_digest: &'a str,
    catalog_digest: &'a str,
}

fn local_mcp_runtime_receipt(launch: LocalMcpServerLaunch) -> LocalMcpServerRuntimeReceipt {
    let input_schema_sha256 = "1".repeat(64);
    let output_schema_sha256 = Some("2".repeat(64));
    let contract_digest = McpToolContractDigest::parse(json_sha256(&ToolContractIdentity {
        planned_runtime_identity: launch.runtime_identity_digest.as_str(),
        name: "read_file",
        input_schema_sha256: &input_schema_sha256,
        output_schema_sha256: output_schema_sha256.as_deref(),
    }))
    .unwrap();
    let tools = vec![LocalMcpToolContractReceipt {
        name: "read_file".into(),
        input_schema_sha256,
        output_schema_sha256,
        contract_digest,
    }];
    let catalog_digest = McpCatalogDigest::parse(json_sha256(&tools)).unwrap();
    let capabilities_digest = McpCapabilitiesDigest::parse("3".repeat(64)).unwrap();
    let runtime_identity_digest = McpRuntimeIdentityDigest::parse(json_sha256(&RuntimeIdentity {
        planned_runtime_identity: launch.runtime_identity_digest.as_str(),
        negotiated_protocol_version: "2025-11-25",
        server_name: "fixture_read",
        server_version: "1.0.0",
        enforced_sandbox_profile_digest: launch.sandbox_profile_digest.as_str(),
        capabilities_digest: capabilities_digest.as_str(),
        catalog_digest: catalog_digest.as_str(),
    }))
    .unwrap();
    LocalMcpServerRuntimeReceipt {
        schema_version: hyper_term_protocol::LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
        credential_scope: launch.credential_scope,
        enforced_sandbox_profile_digest: launch.sandbox_profile_digest.clone(),
        launch,
        negotiated_protocol_version: "2025-11-25".into(),
        server_name: "fixture_read".into(),
        server_version: "1.0.0".into(),
        capabilities_digest,
        catalog_digest,
        runtime_identity_digest,
        tools,
        per_call_isolation: false,
    }
}

#[test]
fn brokered_mcp_execution_is_operation_bound_and_idempotent() {
    let temporary = tempdir().unwrap();
    let state = DaemonState::open(temporary.path().join("state")).unwrap();
    let task_id = state.create_task("brokered MCP execution".into()).unwrap();
    state
        .register_brokered_mcp_runtime(task_id, BrokeredMcpRuntimeConfig::default())
        .unwrap();
    let arguments = serde_json::json!({
        "method": "textDocument/documentSymbol",
        "documentPath": "src/main.ts"
    });
    let proposal_digest = mcp_json_sha256(&serde_json::json!({
        "name": "hyper_term.lsp.query",
        "arguments": arguments,
    }));
    let operation = state
        .propose_brokered_mcp_tool(task_id, "hyper_term.lsp.query".into(), arguments.clone())
        .unwrap();
    let authorized = state
        .decide_permission(
            task_id,
            operation.operation_id,
            operation.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let dispatching = state
        .begin_operation(task_id, operation.operation_id, authorized.revision)
        .unwrap();
    let first = state
        .execute_brokered_mcp_tool(
            task_id,
            operation.operation_id,
            dispatching.revision,
            "hyper_term.lsp.query".into(),
            proposal_digest.clone(),
            arguments.clone(),
        )
        .unwrap();
    assert!(first.is_error);
    assert_eq!(first.outcome, OperationOutcome::Failed);
    assert!(first.text.contains("not configured"));

    let replay = state
        .execute_brokered_mcp_tool(
            task_id,
            operation.operation_id,
            dispatching.revision,
            "hyper_term.lsp.query".into(),
            proposal_digest.clone(),
            arguments,
        )
        .unwrap();
    assert_eq!(replay, first);

    assert!(matches!(
        state.execute_brokered_mcp_tool(
            task_id,
            operation.operation_id,
            dispatching.revision,
            "hyper_term.lsp.query".into(),
            proposal_digest,
            serde_json::json!({
                "method": "textDocument/documentSymbol",
                "documentPath": "src/other.ts"
            }),
        ),
        Err(DaemonError::BrokeredMcpBindingMismatch)
    ));
}

#[cfg(target_os = "macos")]
use hyper_term_sandbox::{LimaImage, LimaRunnerConfig, LimaTaskRunner};

#[cfg(target_os = "macos")]
fn shell(script: &str, cwd: &Path) -> OperationAction {
    OperationAction::Shell {
        command: TerminalCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: Some(cwd.to_path_buf()),
            env: BTreeMap::new(),
        },
    }
}

#[cfg(target_os = "macos")]
fn propose_shell(
    state: &DaemonState,
    task_id: hyper_term_protocol::TaskId,
    script: &str,
    cwd: &Path,
) -> hyper_term_core::OperationRecord {
    state
        .propose_operation(
            task_id,
            OperationKind::Shell,
            shell(script, cwd),
            "run an exact test command".into(),
            RiskClass::ReadOnly,
            vec!["shell".into()],
        )
        .expect("propose operation")
}

#[test]
#[cfg(target_os = "macos")]
fn approval_detail_digest_binds_the_review_and_redacts_environment_values() {
    let temporary = tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = DaemonState::open(temporary.path().join("state")).unwrap();
    let task_id = state.create_task("review exact command".into()).unwrap();
    let operation = state
        .propose_operation(
            task_id,
            OperationKind::Shell,
            OperationAction::Shell {
                command: TerminalCommand {
                    program: "/usr/bin/env".into(),
                    args: vec!["--token".into(), "argument-secret".into()],
                    cwd: Some(workspace),
                    env: BTreeMap::from([("MODEL_CONFIG".into(), "environment-secret".into())]),
                },
            },
            "inspect the exact command".into(),
            RiskClass::ExternalEffect,
            vec!["shell".into()],
        )
        .unwrap();
    let approval = state.approval_detail(operation.operation_id).unwrap();
    let snapshot = serde_json::to_string(&state.block_snapshot(task_id).unwrap()).unwrap();
    assert!(snapshot.contains("<redacted>"));
    assert!(snapshot.contains("MODEL_CONFIG"));
    assert!(!snapshot.contains("argument-secret"));
    assert!(!snapshot.contains("environment-secret"));

    let wrong = ApprovalDetailDigest::parse("f".repeat(64)).unwrap();
    assert!(matches!(
        state.decide_permission_bound(
            task_id,
            operation.operation_id,
            operation.revision,
            &wrong,
            PermissionDecision::AllowOnce,
        ),
        Err(DaemonError::ApprovalDetailMismatch)
    ));
    assert_eq!(
        state
            .approval_detail(operation.operation_id)
            .unwrap()
            .detail
            .operation_revision,
        operation.revision
    );
    assert_eq!(
        state
            .decide_permission_bound(
                task_id,
                operation.operation_id,
                operation.revision,
                &approval.detail_digest,
                PermissionDecision::AllowOnce,
            )
            .unwrap()
            .state,
        OperationState::Authorized
    );
}

#[test]
fn brokered_mcp_approval_shows_canonical_arguments_and_rejects_substitution() {
    let temporary = tempdir().unwrap();
    let state = DaemonState::open(temporary.path().join("state")).unwrap();
    let task_id = state.create_task("review exact MCP call".into()).unwrap();
    let arguments = serde_json::json!({
        "documentPath": "src/main.ts",
        "method": "textDocument/hover",
        "position": {"character": 7, "line": 3}
    });
    let arguments_digest = McpArgumentsDigest::parse(mcp_json_sha256(&arguments)).unwrap();
    let proposal_digest = mcp_json_sha256(&serde_json::json!({
        "name": "hyper_term.lsp.query",
        "arguments": arguments,
    }));
    let operation = state
        .propose_brokered_mcp_tool(task_id, "hyper_term.lsp.query".into(), arguments.clone())
        .unwrap();
    let approval = state.approval_detail(operation.operation_id).unwrap();
    let approval_detail_digest = approval.detail_digest.clone();
    let ApprovalActionDetail::BrokeredMcpTool {
        canonical_arguments_preview,
        arguments_bytes,
        arguments_truncated,
        arguments_digest: reviewed_arguments_digest,
        proposal_digest: reviewed_proposal_digest,
        ..
    } = approval.detail.action
    else {
        panic!("expected a brokered MCP approval");
    };
    assert!(canonical_arguments_preview.contains("textDocument/hover"));
    assert!(canonical_arguments_preview.contains("src/main.ts"));
    assert!(arguments_bytes > 0);
    assert!(!arguments_truncated);
    assert_eq!(reviewed_arguments_digest, arguments_digest);
    assert_eq!(reviewed_proposal_digest, proposal_digest);

    let authorized = state
        .decide_permission_bound(
            task_id,
            operation.operation_id,
            operation.revision,
            &approval_detail_digest,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let dispatching = state
        .begin_operation(task_id, operation.operation_id, authorized.revision)
        .unwrap();
    assert!(matches!(
        state.execute_brokered_mcp_tool(
            task_id,
            operation.operation_id,
            dispatching.revision,
            "hyper_term.lsp.query".into(),
            proposal_digest,
            serde_json::json!({
                "documentPath": "src/secrets.ts",
                "method": "textDocument/hover"
            }),
        ),
        Err(DaemonError::BrokeredMcpBindingMismatch)
    ));
}

#[test]
fn brokered_mcp_journal_keeps_only_a_labelled_bounded_argument_preview() {
    let temporary = tempdir().unwrap();
    let state = DaemonState::open(temporary.path().join("state")).unwrap();
    let task_id = state
        .create_task("review bounded GenUI source".into())
        .unwrap();
    let source = format!(
        "export default function App() {{ return <pre>{}</pre>; }}//TAIL_PRIVATE_MARKER",
        "visible-preview-".repeat(64)
    );
    let operation = state
        .propose_brokered_mcp_tool(
            task_id,
            "hyper_term.genui.compile".into(),
            serde_json::json!({"source": source, "entry": "/App.tsx"}),
        )
        .unwrap();
    let durable_operation = serde_json::to_string(&operation.action).unwrap();
    assert!(durable_operation.contains("canonical_arguments_preview"));
    assert!(durable_operation.contains("arguments_truncated\":true"));
    assert!(!durable_operation.contains("TAIL_PRIVATE_MARKER"));

    let approval = state.approval_detail(operation.operation_id).unwrap();
    let ApprovalActionDetail::BrokeredMcpTool {
        arguments_bytes,
        arguments_truncated,
        canonical_arguments_preview,
        ..
    } = approval.detail.action
    else {
        panic!("expected a brokered MCP approval");
    };
    assert!(arguments_bytes > canonical_arguments_preview.len() as u32);
    assert!(arguments_truncated);
    assert!(canonical_arguments_preview.ends_with('…'));
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
fn fake_lima_runner(root: &Path) -> (LimaTaskRunner, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let executable = root.join("limactl");
    let log = root.join("limactl.log");
    let environment_marker = root.join("limactl-environment");
    let script = format!(
        "#!/bin/sh\nset -eu\nif [ \"${{1:-}}\" = \"--version\" ]; then echo 'limactl version 2.1.1'; exit 0; fi\naction=''\nlast=''\nfor argument in \"$@\"; do\n  last=\"$argument\"\n  case \"$argument\" in validate|start|shell|stop|delete) [ -n \"$action\" ] || action=\"$argument\";; esac\ndone\nprintf '%s\\n' \"$action\" >> '{}'\nif [ \"$action\" = start ]; then\n  printf '%s\\n' \"${{last%/*}}\" > '{}'\nfi\nif [ \"$action\" = shell ]; then\n  environment=$(cat '{}')\n  printf '\\377\\000\\001' > \"$environment/worktree/data.bin\"\n  printf 'isolated only\\n' > \"$environment/worktree/generated.txt\"\n  rm \"$environment/worktree/README.md\"\n  printf 'tier2 stream\\n'\nfi\n",
        log.display(),
        environment_marker.display(),
        environment_marker.display()
    );
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let image = root.join("image.qcow2");
    std::fs::write(&image, b"local pinned image").unwrap();
    let config = LimaRunnerConfig {
        image: LimaImage {
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
    };
    (
        LimaTaskRunner::with_executable(executable, config).unwrap(),
        log,
    )
}

#[test]
#[cfg(target_os = "macos")]
fn tier2_dispatch_consumes_approval_retains_review_result_and_never_edits_workspace() {
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("repository");
    std::fs::create_dir(&workspace).unwrap();
    run_git(&workspace, &["init", "-q"]);
    run_git(&workspace, &["config", "user.name", "Hyper Term Test"]);
    run_git(
        &workspace,
        &["config", "user.email", "hyper-term@example.invalid"],
    );
    std::fs::write(workspace.join("README.md"), "source\n").unwrap();
    run_git(&workspace, &["add", "."]);
    run_git(&workspace, &["commit", "-qm", "fixture"]);

    let state_path = directory.path().join("daemon-state");
    let state = DaemonState::open(&state_path).unwrap();
    let task_id = state.create_task("isolated task".into()).unwrap();
    let operation = state
        .propose_operation(
            task_id,
            OperationKind::Shell,
            shell("printf generated > generated.txt", &workspace),
            "run in an exact-commit VM".into(),
            RiskClass::WorkspaceWrite,
            vec!["shell".into(), "sandbox.isolated_task".into()],
        )
        .unwrap();
    let authorized = state
        .decide_permission(
            task_id,
            operation.operation_id,
            operation.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    assert!(matches!(
        state.dispatch_terminal(
            task_id,
            operation.operation_id,
            authorized.revision,
            TerminalSize::default()
        ),
        Err(DaemonError::IsolatedTaskRequiresVmDispatch)
    ));

    let (runner, log) = fake_lima_runner(directory.path());
    let receipt = state
        .dispatch_isolated_task(
            task_id,
            operation.operation_id,
            authorized.revision,
            &runner,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(receipt.stdout, "tier2 stream\n");
    assert_eq!(receipt.changes.changed_files.len(), 3);
    assert_eq!(
        receipt.changes.changed_files[0].path,
        Path::new("README.md")
    );
    assert_eq!(
        receipt.changes.changed_files[0].kind,
        hyper_term_sandbox::IsolatedChangeKind::Deleted
    );
    assert!(receipt.changes.changed_files[0].content_sha256.is_none());
    assert_eq!(receipt.changes.changed_files[1].path, Path::new("data.bin"));
    let binary_digest = receipt.changes.changed_files[1]
        .content_sha256
        .as_deref()
        .unwrap();
    assert_eq!(
        state
            .read_isolated_result_file(operation.operation_id, Path::new("data.bin"), binary_digest)
            .unwrap(),
        [255, 0, 1]
    );
    assert_eq!(
        receipt.changes.changed_files[2].path,
        Path::new("generated.txt")
    );
    let generated_digest = receipt.changes.changed_files[2]
        .content_sha256
        .as_deref()
        .unwrap();
    assert_eq!(
        state
            .read_isolated_result_file(
                operation.operation_id,
                Path::new("generated.txt"),
                generated_digest
            )
            .unwrap(),
        b"isolated only\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("README.md")).unwrap(),
        "source\n"
    );
    assert!(!workspace.join("data.bin").exists());
    assert!(!workspace.join("generated.txt").exists());
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "validate\nstart\nshell\nstop\ndelete\n"
    );
    drop(state);

    let detached_workspace = directory.path().join("repository-detached");
    std::fs::rename(&workspace, &detached_workspace).unwrap();
    let state = DaemonState::open(&state_path)
        .expect("an unavailable retained Tier 2 result must not block daemon startup");
    let unavailable = state.isolated_result_receipt(operation.operation_id);
    assert!(
        matches!(unavailable, Err(DaemonError::IsolatedResultMissing(id)) if id == operation.operation_id)
    );
    let retained_result = state_path
        .join("isolated-results")
        .join(operation.operation_id.to_string());
    assert!(
        retained_result.is_dir(),
        "the unavailable result remains durable for later recovery"
    );
    drop(state);
    std::fs::rename(&detached_workspace, &workspace).unwrap();

    let state = DaemonState::open(&state_path).unwrap();
    let recovered = state
        .isolated_result_receipt(operation.operation_id)
        .unwrap();
    assert_eq!(recovered.changes, receipt.changes);
    assert_eq!(
        state
            .read_isolated_result_file(
                operation.operation_id,
                Path::new("generated.txt"),
                generated_digest
            )
            .unwrap(),
        b"isolated only\n"
    );
    let preview = state
        .preview_isolated_result_acceptance(task_id, operation.operation_id)
        .unwrap();
    assert_eq!(
        preview.target_paths,
        vec!["README.md", "data.bin", "generated.txt"]
    );
    assert_eq!(preview.changes[0].target_path, "README.md");
    assert_eq!(preview.changes[0].before, "source\n");
    assert_eq!(preview.changes[0].after, "");
    assert!(preview.changes[0].deleted);
    assert_eq!(preview.changes[1].target_path, "data.bin");
    assert!(preview.changes[1].binary);
    assert_eq!(preview.changes[1].base_bytes, 0);
    assert_eq!(preview.changes[1].proposed_bytes, 3);
    assert!(preview.changes[1].before.is_empty());
    assert!(preview.changes[1].after.is_empty());
    assert_eq!(preview.changes[2].target_path, "generated.txt");
    assert_eq!(preview.changes[2].before, "");
    assert_eq!(preview.changes[2].after, "isolated only\n");
    assert!(!preview.changes[2].deleted);
    assert!(!preview.changes[2].binary);
    assert!(
        state
            .isolated_acceptance_reviews(task_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        std::fs::read_dir(state_path.join("isolated-acceptances"))
            .unwrap()
            .next()
            .is_none()
    );
    let acceptance = state
        .propose_isolated_result_acceptance(task_id, operation.operation_id)
        .unwrap();
    assert_eq!(
        acceptance.target_paths,
        vec!["README.md", "data.bin", "generated.txt"]
    );
    assert!(acceptance.operation.summary.contains("README.md"));
    assert!(acceptance.operation.summary.contains("data.bin"));
    assert!(acceptance.operation.summary.contains("generated.txt"));
    let acceptance_path = state_path
        .join("isolated-acceptances")
        .join(format!("{}.json", acceptance.operation.operation_id));
    assert_eq!(
        std::fs::metadata(&acceptance_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!workspace.join("data.bin").exists());
    assert!(!workspace.join("generated.txt").exists());
    drop(state);

    std::fs::rename(&workspace, &detached_workspace).unwrap();
    let state = DaemonState::open(&state_path)
        .expect("an unavailable reviewed Tier 2 result must not block daemon startup");
    let unavailable_acceptances = state.isolated_acceptance_reviews(task_id).unwrap();
    assert!(
        unavailable_acceptances.is_empty(),
        "an acceptance without a validated source result is not actionable"
    );
    assert!(
        acceptance_path.is_file(),
        "the unavailable acceptance remains durable"
    );
    drop(state);
    std::fs::rename(&detached_workspace, &workspace).unwrap();

    let stored_acceptance = std::fs::read_to_string(&acceptance_path).unwrap();
    let mut tampered: serde_json::Value = serde_json::from_str(&stored_acceptance).unwrap();
    tampered["workspace"] =
        serde_json::Value::String(workspace.join("other").display().to_string());
    std::fs::write(&acceptance_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    assert!(matches!(
        DaemonState::open(&state_path),
        Err(DaemonError::InvalidIsolatedAcceptanceStore)
    ));
    std::fs::write(&acceptance_path, stored_acceptance).unwrap();
    let state = DaemonState::open(&state_path).unwrap();
    let recovered_acceptance = state
        .isolated_acceptance_review(acceptance.operation.operation_id)
        .unwrap();
    assert_eq!(recovered_acceptance, acceptance);
    assert!(matches!(
        state.accept_isolated_result(
            task_id,
            acceptance.operation.operation_id,
            acceptance.operation.revision
        ),
        Err(DaemonError::OperationNotAuthorized(_))
    ));
    let authorized_acceptance = state
        .decide_isolated_acceptance_permission(
            task_id,
            acceptance.operation.operation_id,
            acceptance.operation.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let completed = state
        .accept_isolated_result(
            task_id,
            acceptance.operation.operation_id,
            authorized_acceptance.revision,
        )
        .unwrap();
    assert_eq!(completed.state, OperationState::Succeeded);
    assert!(!acceptance_path.exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("generated.txt")).unwrap(),
        "isolated only\n"
    );
    assert_eq!(
        std::fs::read(workspace.join("data.bin")).unwrap(),
        [255, 0, 1]
    );
    assert!(!workspace.join("README.md").exists());
    assert!(matches!(
        state.discard_isolated_result(operation.operation_id),
        Err(DaemonError::IsolatedResultMissing(id)) if id == operation.operation_id
    ));
}

#[test]
#[cfg(target_os = "macos")]
fn terminal_dispatch_requires_permission_and_survives_client_subscription_drop() {
    let directory = tempdir().expect("tempdir");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = DaemonState::open(directory.path().join("state")).expect("open daemon");
    let task_id = state.create_task("permission boundary".into()).unwrap();
    let proposed = propose_shell(
        &state,
        task_id,
        "sleep 0.05; printf durable-marker",
        &workspace,
    );
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

    let events = std::fs::read_to_string(directory.path().join("state/events.jsonl")).unwrap();
    assert!(events.contains("sandbox_profile_compiled"));
    assert!(events.contains("sandbox_lease_issued"));
    assert!(events.contains("sandbox_receipt_recorded"));
    assert!(events.contains("mac_os_seatbelt"));
    assert!(events.contains("\"enforced\":true"));
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
                        outcome: Some(OperationOutcome::Succeeded),
                        ..
                    } if *operation_id == proposed.operation_id
                        && executor == "sandbox::MacOsSeatbelt"
                )
            })
    );

    let digest = state.block_snapshot(task_id).unwrap().semantic_digest;
    drop(state);
    // The detached terminal monitor owns the last daemon clone until it observes
    // the PTY exit barrier. A restart must wait for that owner to release the
    // state root instead of racing a second journal writer into the directory.
    let restart_deadline = Instant::now() + Duration::from_secs(3);
    let reopened = loop {
        match DaemonState::open(directory.path().join("state")) {
            Ok(reopened) => break reopened,
            Err(DaemonError::StateDirectoryInUse(_)) if Instant::now() < restart_deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("replay journal: {error}"),
        }
    };
    assert_eq!(
        reopened.block_snapshot(task_id).unwrap().semantic_digest,
        digest
    );
}

#[test]
#[cfg(target_os = "macos")]
fn agent_workspace_writes_require_the_matching_risk_profile() {
    let directory = tempdir().expect("tempdir");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = DaemonState::open(directory.path().join("state")).unwrap();

    let read_task = state.create_task("read only Agent".into()).unwrap();
    let read_operation = state
        .propose_operation(
            read_task,
            OperationKind::Shell,
            shell("printf denied > denied.txt", &workspace),
            "attempt a write under a read-only profile".into(),
            RiskClass::ReadOnly,
            vec!["shell".into()],
        )
        .unwrap();
    let read_authorized = state
        .decide_permission(
            read_task,
            read_operation.operation_id,
            read_operation.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    state
        .dispatch_terminal(
            read_task,
            read_operation.operation_id,
            read_authorized.revision,
            TerminalSize::default(),
        )
        .unwrap();
    wait_for_operation_state(
        &state,
        read_task,
        read_operation.operation_id,
        OperationState::Failed,
    );
    assert!(!workspace.join("denied.txt").exists());

    let write_task = state.create_task("workspace writer Agent".into()).unwrap();
    let write_operation = state
        .propose_operation(
            write_task,
            OperationKind::Shell,
            shell("printf allowed > allowed.txt", &workspace),
            "write one workspace file".into(),
            RiskClass::WorkspaceWrite,
            vec!["shell".into()],
        )
        .unwrap();
    let write_authorized = state
        .decide_permission(
            write_task,
            write_operation.operation_id,
            write_operation.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    state
        .dispatch_terminal(
            write_task,
            write_operation.operation_id,
            write_authorized.revision,
            TerminalSize::default(),
        )
        .unwrap();
    wait_for_operation_state(
        &state,
        write_task,
        write_operation.operation_id,
        OperationState::Succeeded,
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("allowed.txt")).unwrap(),
        "allowed"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn daemon_restart_invalidates_an_unused_sandbox_lease() {
    let directory = tempdir().expect("tempdir");
    let state_path = directory.path().join("state");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = DaemonState::open(&state_path).unwrap();
    let task_id = state.create_task("restart lease".into()).unwrap();
    let operation = propose_shell(&state, task_id, "printf never-run", &workspace);
    let authorized = state
        .decide_permission(
            task_id,
            operation.operation_id,
            operation.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    assert_eq!(authorized.state, OperationState::Authorized);
    drop(state);

    let reopened = DaemonState::open(&state_path).unwrap();
    wait_for_operation_state(
        &reopened,
        task_id,
        operation.operation_id,
        OperationState::Failed,
    );
    let events = std::fs::read_to_string(state_path.join("events.jsonl")).unwrap();
    let context_receipt = events
        .lines()
        .filter_map(|line| serde_json::from_str::<EventEnvelope>(line).ok())
        .find_map(|event| match event.payload {
            DomainEvent::OperationExecutionContextCompiled {
                operation_revision,
                receipt,
            } if event.operation_id == Some(operation.operation_id) => {
                assert_eq!(operation_revision, authorized.revision);
                Some(receipt)
            }
            _ => None,
        })
        .expect("durable execution context receipt");
    assert_eq!(context_receipt.context_revision, authorized.revision);
    assert_eq!(context_receipt.context_digest.as_str().len(), 64);
    assert_eq!(context_receipt.environment_digest.as_str().len(), 64);
    assert!(context_receipt.clear_inherited);
    assert!(events.contains("restart invalidated the in-memory one-use sandbox lease"));
}

#[test]
fn sandbox_approval_requires_an_explicit_workspace() {
    let directory = tempdir().expect("tempdir");
    let state = DaemonState::open(directory.path().join("state")).unwrap();
    let task_id = state.create_task("missing workspace".into()).unwrap();
    let proposed = state
        .propose_operation(
            task_id,
            OperationKind::Shell,
            OperationAction::Shell {
                command: TerminalCommand {
                    program: "/bin/true".into(),
                    args: Vec::new(),
                    cwd: None,
                    env: BTreeMap::new(),
                },
            },
            "missing cwd".into(),
            RiskClass::ReadOnly,
            vec!["shell".into()],
        )
        .unwrap();
    assert!(matches!(
        state.decide_permission(
            task_id,
            proposed.operation_id,
            proposed.revision,
            PermissionDecision::AllowOnce,
        ),
        Err(DaemonError::SandboxWorkingDirectoryRequired)
    ));
    let snapshot = state.block_snapshot(task_id).unwrap();
    assert!(snapshot.blocks.iter().any(|block| {
        matches!(
            block.payload,
            BlockPayload::Operation {
                operation_id,
                state: OperationState::WaitingHuman,
                ..
            } if operation_id == proposed.operation_id
        )
    }));
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
                outcome: Some(OperationOutcome::Succeeded),
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
                        outcome: Some(OperationOutcome::Succeeded),
                        ..
                    } if *operation_id == proposed.operation_id && executor == "hyper-term-mcp"
                )
            })
    );
}

#[test]
fn local_mcp_server_launch_is_separately_authorized_and_redacted() {
    let directory = tempdir().expect("tempdir");
    let state_path = directory.path().join("state");
    let state = DaemonState::open(&state_path).expect("open daemon");
    let task_id = state.create_task("Reviewed local MCP".into()).unwrap();
    let executable = std::fs::canonicalize("/bin/sh").unwrap();
    let executable_sha256 = Sha256::digest(std::fs::read(&executable).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let working_directory = directory.path().canonicalize().unwrap();
    let launch = LocalMcpServerLaunch {
        schema_version: hyper_term_protocol::LOCAL_MCP_LAUNCH_SCHEMA_VERSION,
        server_id: "fixture_read".into(),
        executable,
        executable_sha256,
        arguments_digest: McpArgumentsDigest::parse("a".repeat(64)).unwrap(),
        argument_count: 2,
        working_directory,
        context_digest: ContextDigest::parse("b".repeat(64)).unwrap(),
        sandbox_profile_digest: SandboxProfileDigest::parse("c".repeat(64)).unwrap(),
        roots_snapshot_sha256: Some("d".repeat(64)),
        lifecycle: LocalMcpServerLifecycle::OneTask,
        credential_scope: LocalMcpCredentialScope::ServerLifetime,
        runtime_identity_digest: McpRuntimeIdentityDigest::parse("e".repeat(64)).unwrap(),
    };
    let proposed = state
        .propose_operation(
            task_id,
            OperationKind::McpServerLaunch,
            OperationAction::McpServerLaunch {
                launch: launch.clone(),
            },
            "Start pinned fixture_read MCP for this Agent task".into(),
            RiskClass::ExternalEffect,
            vec!["mcp.server.launch".into()],
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
    let receipt = local_mcp_runtime_receipt(launch.clone());
    let mut false_isolation_claim = receipt.clone();
    false_isolation_claim.per_call_isolation = true;
    assert!(matches!(
        state.record_local_mcp_server_runtime(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            false_isolation_claim,
        ),
        Err(DaemonError::InvalidLocalMcpRuntimeReceipt)
    ));
    let mut mismatched_launch = receipt.clone();
    mismatched_launch.launch.server_id = "unreviewed-server".into();
    assert!(matches!(
        state.record_local_mcp_server_runtime(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            mismatched_launch,
        ),
        Err(DaemonError::InvalidLocalMcpRuntimeReceipt)
    ));
    assert!(matches!(
        state.record_local_mcp_server_runtime(
            task_id,
            proposed.operation_id,
            dispatching.revision - 1,
            receipt.clone(),
        ),
        Err(DaemonError::StaleOperationRevision { .. })
    ));
    state
        .record_local_mcp_server_runtime(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            receipt.clone(),
        )
        .unwrap();
    assert!(matches!(
        state.record_local_mcp_server_runtime(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            receipt.clone(),
        ),
        Err(DaemonError::InvalidLocalMcpRuntimeReceipt)
    ));
    let runtime_event = state
        .local_mcp_server_runtime_event(task_id, proposed.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(runtime_event.operation_id, Some(proposed.operation_id));
    assert_eq!(runtime_event.causation_id, runtime_event.correlation_id);
    assert!(runtime_event.causation_id.is_some());
    assert!(matches!(
        &runtime_event.payload,
        DomainEvent::LocalMcpServerRuntimeRecorded { receipt: recorded }
            if recorded == &receipt
    ));
    drop(state);

    let reopened = DaemonState::open(&state_path).expect("reopen daemon");
    let replayed = reopened
        .local_mcp_server_runtime_event(task_id, proposed.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(replayed.event_id, runtime_event.event_id);
    assert!(matches!(
        reopened.complete_operation(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            OperationCompletion {
                executor: "hyper-term-mcp-client".into(),
                succeeded: true,
                outcome: Some(OperationOutcome::Succeeded),
                summary: "stale pre-restart completion".into(),
                result_digest: Some(launch.runtime_identity_digest.to_string()),
            },
        ),
        Err(DaemonError::StaleOperationRevision { .. })
    ));
    reopened
        .complete_operation(
            task_id,
            proposed.operation_id,
            dispatching.revision + 1,
            OperationCompletion {
                executor: "hyper-term-mcp-client".into(),
                succeeded: true,
                outcome: Some(OperationOutcome::Succeeded),
                summary: "MCP initialized and its catalog identity was recorded".into(),
                result_digest: Some(launch.runtime_identity_digest.to_string()),
            },
        )
        .unwrap();

    let tool_call = LocalMcpToolCall {
        schema_version: hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION,
        server_id: receipt.launch.server_id.clone(),
        runtime_identity_digest: receipt.runtime_identity_digest.clone(),
        catalog_digest: receipt.catalog_digest.clone(),
        tool_name: receipt.tools[0].name.clone(),
        tool_contract_digest: receipt.tools[0].contract_digest.clone(),
        arguments_digest: McpArgumentsDigest::parse("8".repeat(64)).unwrap(),
    };
    let missing_runtime_call = LocalMcpToolCall {
        runtime_identity_digest: McpRuntimeIdentityDigest::parse("9".repeat(64)).unwrap(),
        ..tool_call.clone()
    };
    let missing_runtime = reopened
        .propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::McpToolCall {
                call: missing_runtime_call.clone(),
            },
            "Invoke a tool on an unrecorded MCP runtime".into(),
            RiskClass::ReadOnly,
            vec!["mcp.tool.call".into()],
        )
        .unwrap();
    let missing_runtime = reopened
        .decide_permission(
            task_id,
            missing_runtime.operation_id,
            missing_runtime.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let missing_runtime = reopened
        .begin_operation(
            task_id,
            missing_runtime.operation_id,
            missing_runtime.revision,
        )
        .unwrap();
    assert!(matches!(
        reopened.record_local_mcp_tool_call(
            task_id,
            missing_runtime.operation_id,
            missing_runtime.revision,
            LocalMcpToolCallReceipt {
                schema_version: hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION,
                call: missing_runtime_call,
                succeeded: true,
                result_digest: McpToolResultDigest::parse("a".repeat(64)).unwrap(),
                content_count: 1,
                has_structured_content: false,
            },
        ),
        Err(DaemonError::LocalMcpRuntimeNotRecorded)
    ));

    let proposed_call = reopened
        .propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::McpToolCall {
                call: tool_call.clone(),
            },
            "Invoke the reviewed read_file tool".into(),
            RiskClass::ReadOnly,
            vec!["mcp.tool.call".into()],
        )
        .unwrap();
    let authorized_call = reopened
        .decide_permission(
            task_id,
            proposed_call.operation_id,
            proposed_call.revision,
            PermissionDecision::AllowOnce,
        )
        .unwrap();
    let dispatching_call = reopened
        .begin_operation(
            task_id,
            proposed_call.operation_id,
            authorized_call.revision,
        )
        .unwrap();
    let tool_receipt = LocalMcpToolCallReceipt {
        schema_version: hyper_term_protocol::LOCAL_MCP_TOOL_CALL_SCHEMA_VERSION,
        call: tool_call,
        succeeded: true,
        result_digest: McpToolResultDigest::parse("f".repeat(64)).unwrap(),
        content_count: 1,
        has_structured_content: true,
    };
    let mut substituted_receipt = tool_receipt.clone();
    substituted_receipt.call.arguments_digest = McpArgumentsDigest::parse("0".repeat(64)).unwrap();
    assert!(matches!(
        reopened.record_local_mcp_tool_call(
            task_id,
            proposed_call.operation_id,
            dispatching_call.revision,
            substituted_receipt,
        ),
        Err(DaemonError::InvalidLocalMcpToolCallReceipt)
    ));
    reopened
        .record_local_mcp_tool_call(
            task_id,
            proposed_call.operation_id,
            dispatching_call.revision,
            tool_receipt.clone(),
        )
        .unwrap();
    assert!(matches!(
        reopened.record_local_mcp_tool_call(
            task_id,
            proposed_call.operation_id,
            dispatching_call.revision,
            tool_receipt.clone(),
        ),
        Err(DaemonError::InvalidLocalMcpToolCallReceipt)
    ));
    let tool_event = reopened
        .local_mcp_tool_call_event(task_id, proposed_call.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(tool_event.operation_id, Some(proposed_call.operation_id));
    assert_eq!(tool_event.correlation_id, Some(runtime_event.event_id));
    assert!(tool_event.causation_id.is_some());
    assert_ne!(tool_event.causation_id, tool_event.correlation_id);
    assert!(matches!(
        &tool_event.payload,
        DomainEvent::LocalMcpToolCallRecorded { receipt: recorded }
            if recorded == &tool_receipt
    ));
    reopened
        .complete_operation(
            task_id,
            proposed_call.operation_id,
            dispatching_call.revision,
            OperationCompletion {
                executor: "hyper-term-mcp-client".into(),
                succeeded: true,
                outcome: Some(OperationOutcome::Succeeded),
                summary: "Reviewed read_file call completed".into(),
                result_digest: Some(tool_receipt.result_digest.to_string()),
            },
        )
        .unwrap();

    let events = std::fs::read_to_string(state_path.join("events.jsonl")).unwrap();
    assert!(events.contains("mcp_server_launch"));
    assert!(events.contains("local_mcp_server_runtime_recorded"));
    assert!(events.contains("2025-11-25"));
    assert!(events.contains("fixture_read"));
    assert!(events.contains("local_mcp_tool_call_recorded"));
    assert!(events.contains("read_file"));
    assert!(!events.contains("secret-token"));
    assert!(matches!(
        reopened.propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::McpServerLaunch { launch },
            "wrong kind".into(),
            RiskClass::ReadOnly,
            Vec::new(),
        ),
        Err(DaemonError::ActionKindMismatch)
    ));
}

#[test]
fn uncertain_tool_completion_is_not_recorded_as_a_definitive_failure() {
    let directory = tempdir().expect("tempdir");
    let state = DaemonState::open(directory.path()).expect("open daemon");
    let task_id = state.create_task("Uncertain MCP tool".into()).unwrap();
    let proposed = state
        .propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::Opaque {
                kind: "hyper_term.genui.compile".into(),
                payload_digest: "a".repeat(64),
            },
            "Compile generated UI".into(),
            RiskClass::ReadOnly,
            vec!["genui_compile".into()],
        )
        .unwrap();
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

    let uncertain = state
        .complete_operation(
            task_id,
            proposed.operation_id,
            dispatching.revision,
            OperationCompletion {
                executor: "hyper-term-mcp".into(),
                succeeded: false,
                outcome: Some(OperationOutcome::UnknownExecution),
                summary: "Deno compiler timed out after dispatch".into(),
                result_digest: Some("b".repeat(64)),
            },
        )
        .unwrap();
    assert_eq!(uncertain.state, OperationState::UnknownExecution);
    let snapshot = state.block_snapshot(task_id).unwrap();
    assert!(snapshot.blocks.iter().any(|block| {
        block.lifecycle == hyper_term_protocol::BlockLifecycle::UnknownExecution
            && matches!(
                block.payload,
                BlockPayload::OperationReceipt {
                    operation_id,
                    outcome: Some(OperationOutcome::UnknownExecution),
                    ..
                } if operation_id == proposed.operation_id
            )
    }));

    let reconciled = state
        .complete_operation(
            task_id,
            proposed.operation_id,
            uncertain.revision,
            OperationCompletion {
                executor: "hyper-term-reconciler".into(),
                succeeded: false,
                outcome: Some(OperationOutcome::Failed),
                summary: "Evidence proved no artifact was accepted".into(),
                result_digest: Some("c".repeat(64)),
            },
        )
        .unwrap();
    assert_eq!(reconciled.state, OperationState::Failed);
}

#[test]
fn only_a_valid_dispatching_genui_compile_replaces_the_last_known_good_artifact() {
    let directory = tempdir().expect("tempdir");
    let state_path = directory.path().join("state");
    let state = DaemonState::open(&state_path).expect("open daemon");
    let task_id = state.create_task("Agentic UI".into()).unwrap();

    let dispatch = |state: &DaemonState| {
        let proposed = state
            .propose_operation(
                task_id,
                OperationKind::McpTool,
                OperationAction::Opaque {
                    kind: "hyper_term.genui.compile".into(),
                    payload_digest: "a".repeat(64),
                },
                "Compile bounded Agentic UI".into(),
                RiskClass::ReadOnly,
                vec!["genui_compile".into()],
            )
            .unwrap();
        let authorized = state
            .decide_permission(
                task_id,
                proposed.operation_id,
                proposed.revision,
                PermissionDecision::AllowOnce,
            )
            .unwrap();
        state
            .begin_operation(task_id, proposed.operation_id, authorized.revision)
            .unwrap()
    };
    let candidate = |revision, bundle: &str| {
        let mut digest = Sha256::new();
        digest.update(bundle.as_bytes());
        GenUiArtifactCandidate {
            schema_version: 1,
            source_revision: revision,
            entrypoint: "/App.tsx".into(),
            source_files: BTreeMap::from([(
                "/App.tsx".into(),
                "export default () => null;".into(),
            )]),
            bundle: bundle.into(),
            css: String::new(),
            source_map: "{}".into(),
            content_digest: digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            compiler: GenUiCompilerIdentity {
                name: "esbuild-wasm".into(),
                version: "0.28.1".into(),
            },
            diagnostics: Vec::new(),
        }
    };

    let first = dispatch(&state);
    let accepted = state
        .accept_genui_artifact(
            task_id,
            first.operation_id,
            first.revision,
            candidate(1, "last-good"),
        )
        .unwrap();
    state
        .complete_operation(
            task_id,
            first.operation_id,
            first.revision,
            OperationCompletion {
                executor: "hyper-term-mcp".into(),
                succeeded: true,
                outcome: Some(OperationOutcome::Succeeded),
                summary: "GenUI artifact accepted".into(),
                result_digest: Some("b".repeat(64)),
            },
        )
        .unwrap();

    let second = dispatch(&state);
    let mut invalid = candidate(2, "broken");
    invalid.content_digest = "0".repeat(64);
    assert!(matches!(
        state.accept_genui_artifact(task_id, second.operation_id, second.revision, invalid),
        Err(DaemonError::ArtifactStore(_))
    ));
    state
        .complete_operation(
            task_id,
            second.operation_id,
            second.revision,
            OperationCompletion {
                executor: "hyper-term-mcp".into(),
                succeeded: false,
                outcome: Some(OperationOutcome::Failed),
                summary: "GenUI artifact rejected".into(),
                result_digest: Some("c".repeat(64)),
            },
        )
        .unwrap();

    let snapshot = state.block_snapshot(task_id).unwrap();
    let artifacts = snapshot
        .blocks
        .iter()
        .filter_map(|block| match &block.payload {
            BlockPayload::Artifact { artifact } => Some(artifact),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].artifact_id, accepted.artifact_id);
    assert_eq!(artifacts[0].source_revision, 1);
    drop(state);

    let reopened = DaemonState::open(&state_path).expect("reopen validates accepted artifact");
    assert!(
        reopened
            .block_snapshot(task_id)
            .unwrap()
            .blocks
            .iter()
            .any(|block| matches!(
                &block.payload,
                BlockPayload::Artifact { artifact } if artifact.artifact_id == accepted.artifact_id
            ))
    );
}

#[test]
#[cfg(target_os = "macos")]
fn unix_client_can_reconnect_and_replay_terminal_output() {
    let directory = tempdir().expect("tempdir");
    let socket = directory.path().join("hyperd.sock");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = DaemonState::open(directory.path().join("state")).unwrap();
    let _server = spawn_unix_server(&socket, state.clone()).expect("spawn server");

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
        action: shell("sleep 0.1; printf socket-reconnect-marker", &workspace),
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
        approval_detail_digest: state.approval_detail(operation_id).unwrap().detail_digest,
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
#[cfg(target_os = "macos")]
fn terminal_input_requires_the_active_client_lease() {
    let directory = tempdir().expect("tempdir");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = DaemonState::open(directory.path().join("state")).expect("open daemon");
    let task_id = state.create_task("input lease".into()).unwrap();
    let proposed = propose_shell(&state, task_id, "cat", &workspace);
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

#[cfg(target_os = "macos")]
fn wait_for_operation_state(
    state: &DaemonState,
    task_id: hyper_term_protocol::TaskId,
    operation_id: hyper_term_protocol::OperationId,
    expected: OperationState,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = state.block_snapshot(task_id).unwrap();
        if snapshot.blocks.iter().any(|block| {
            matches!(
                block.payload,
                BlockPayload::Operation {
                    operation_id: id,
                    state,
                    ..
                } if id == operation_id && state == expected
            )
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "operation {operation_id} did not reach {expected:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
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
