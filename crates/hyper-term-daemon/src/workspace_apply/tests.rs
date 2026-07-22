
use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;

#[test]
fn existing_regular_file_is_replaced_atomically_and_keeps_its_mode() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("App.tsx");
    fs::write(&target, "export const value = 'before';\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

    let plan = prepare_workspace_apply(
        &workspace,
        "App.tsx",
        "export const value = 'after';\n".into(),
    )
    .unwrap();
    assert_eq!(plan.base_content(), "export const value = 'before';\n");
    assert_eq!(plan.base_digest().map(str::len), Some(64));
    let digest = apply_workspace_plan(&workspace, &plan).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), plan.proposed_content);
    assert_eq!(digest, plan.proposed_digest);
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(fs::read_dir(&workspace).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".hyper-term-apply-")
    }));
}

#[test]
fn reviewed_deletion_unlinks_only_the_exact_file_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("obsolete.txt");
    fs::write(&target, "reviewed content\n").unwrap();

    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![WorkspaceApplyRequest::Delete {
            target_path: "obsolete.txt".into(),
        }],
    )
    .unwrap();
    let plan = &set.plans[0];
    assert!(plan.deletes_target());
    assert_eq!(plan.base_content(), "reviewed content\n");
    assert!(plan.proposed_content.is_empty());

    let digest = apply_workspace_plan(&workspace, plan).unwrap();
    assert_eq!(digest, set.result_digest);
    assert!(!target.exists());
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn stale_file_identity_blocks_deletion_even_when_the_text_is_equal() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("obsolete.txt");
    fs::write(&target, "reviewed content\n").unwrap();
    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![WorkspaceApplyRequest::Delete {
            target_path: "obsolete.txt".into(),
        }],
    )
    .unwrap();
    let replacement = workspace.join("replacement.txt");
    fs::write(&replacement, "reviewed content\n").unwrap();
    fs::rename(&replacement, &target).unwrap();

    assert!(matches!(
        apply_workspace_plan(&workspace, &set.plans[0]),
        Err(WorkspaceApplyError::StaleBase)
    ));
    assert_eq!(fs::read_to_string(target).unwrap(), "reviewed content\n");
}

#[test]
fn ordinary_write_plan_serialization_keeps_the_v1_acceptance_shape() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let plan = prepare_workspace_apply(&workspace, "new.txt", "new\n".into()).unwrap();

    let json = serde_json::to_string(&plan).unwrap();
    assert!(!json.contains("\"delete\""));
    assert!(!json.contains("proposed_binary_base64"));
    assert!(!json.contains("binary_bytes"));
    let recovered: WorkspaceApplyPlan = serde_json::from_str(&json).unwrap();
    assert!(!recovered.deletes_target());
}

#[test]
fn non_utf8_file_is_encoded_canonically_and_applied_as_exact_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let proposed = vec![0, 159, 146, 150, 255];
    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![WorkspaceApplyRequest::WriteBytes {
            target_path: "image.bin".into(),
            proposed_bytes: proposed.clone(),
        }],
    )
    .unwrap();
    let plan = &set.plans[0];

    assert!(plan.is_binary());
    assert_eq!(plan.base_bytes_len(), 0);
    assert_eq!(plan.proposed_bytes_len(), proposed.len() as u64);
    assert!(plan.proposed_content.is_empty());
    let json = serde_json::to_string(plan).unwrap();
    assert!(json.contains(&format!(
        "\"proposed_binary_base64\":\"{}\"",
        BASE64_STANDARD.encode(&proposed)
    )));
    let mut ambiguous = set.clone();
    ambiguous.plans[0].proposed_content = "shadow text".into();
    assert!(matches!(
        validate_workspace_apply_set(&ambiguous),
        Err(WorkspaceApplyError::InvalidPath)
    ));

    apply_workspace_plan(&workspace, plan).unwrap();
    assert_eq!(fs::read(workspace.join("image.bin")).unwrap(), proposed);
}

#[test]
fn reviewed_binary_deletion_preserves_only_identity_until_unlink() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("obsolete.bin");
    fs::write(&target, [255, 0, 1]).unwrap();
    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![WorkspaceApplyRequest::Delete {
            target_path: "obsolete.bin".into(),
        }],
    )
    .unwrap();
    let plan = &set.plans[0];

    assert!(plan.deletes_target());
    assert!(plan.is_binary());
    assert_eq!(plan.base_bytes_len(), 3);
    assert!(!serde_json::to_string(plan).unwrap().contains("/wAB"));
    apply_workspace_plan(&workspace, plan).unwrap();
    assert!(!target.exists());
}

#[test]
fn binary_base_keeps_only_bounded_identity_metadata_in_the_review_plan() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("image.bin");
    let base = vec![255, 0, 1, 2, 3];
    let proposed = vec![254, 4, 5, 6];
    fs::write(&target, &base).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![WorkspaceApplyRequest::WriteBytes {
            target_path: "image.bin".into(),
            proposed_bytes: proposed.clone(),
        }],
    )
    .unwrap();
    let plan = &set.plans[0];

    assert!(plan.is_binary());
    assert_eq!(plan.base_bytes_len(), base.len() as u64);
    let json = serde_json::to_string(plan).unwrap();
    assert!(json.contains(&format!("\"binary_bytes\":{}", base.len())));
    assert!(!json.contains(&BASE64_STANDARD.encode(&base)));

    apply_workspace_plan(&workspace, plan).unwrap();
    assert_eq!(fs::read(&target).unwrap(), proposed);
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn stale_binary_identity_blocks_the_apply() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("image.bin");
    fs::write(&target, [255, 0, 1]).unwrap();
    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![WorkspaceApplyRequest::WriteBytes {
            target_path: "image.bin".into(),
            proposed_bytes: vec![254, 2, 3],
        }],
    )
    .unwrap();
    let replacement = workspace.join("replacement.bin");
    fs::write(&replacement, [255, 0, 1]).unwrap();
    fs::rename(&replacement, &target).unwrap();

    assert!(matches!(
        apply_workspace_plan(&workspace, &set.plans[0]),
        Err(WorkspaceApplyError::StaleBase)
    ));
    assert_eq!(fs::read(target).unwrap(), [255, 0, 1]);
}

#[test]
fn stale_file_identity_blocks_the_apply_even_when_the_text_is_equal() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let target = workspace.join("main.ts");
    fs::write(&target, "const before = true;\n").unwrap();
    let plan =
        prepare_workspace_apply(&workspace, "main.ts", "const after = true;\n".into()).unwrap();
    let replacement = workspace.join("replacement.ts");
    fs::write(&replacement, "const before = true;\n").unwrap();
    fs::rename(&replacement, &target).unwrap();

    assert!(matches!(
        apply_workspace_plan(&workspace, &plan),
        Err(WorkspaceApplyError::StaleBase)
    ));
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "const before = true;\n"
    );
}

#[test]
fn missing_target_is_created_only_if_it_remains_missing() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    let plan = prepare_workspace_apply(
        &workspace,
        "new.ts",
        "export const created = true;\n".into(),
    )
    .unwrap();
    assert!(plan.base.is_none());
    fs::write(workspace.join("new.ts"), "external writer\n").unwrap();

    assert!(matches!(
        apply_workspace_plan(&workspace, &plan),
        Err(WorkspaceApplyError::StaleBase)
    ));
    assert_eq!(
        fs::read_to_string(workspace.join("new.ts")).unwrap(),
        "external writer\n"
    );
}

#[test]
fn traversal_vcs_metadata_and_symlink_parents_are_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let outside = temporary.path().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, workspace.join("linked")).unwrap();
    let workspace = workspace.canonicalize().unwrap();

    for target in [
        "../outside.ts",
        ".git/config",
        "linked/escape.ts",
        "/tmp/a.ts",
    ] {
        assert!(prepare_workspace_apply(&workspace, target, "x".into()).is_err());
    }
    assert!(!outside.join("escape.ts").exists());
}

#[test]
fn multi_file_set_installs_all_reviewed_targets_and_cleans_backups() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/App.tsx"), "before app\n").unwrap();
    fs::write(workspace.join("src/theme.ts"), "before theme\n").unwrap();

    let set = prepare_workspace_apply_set(
        &workspace,
        vec![
            ("src/App.tsx".into(), "after app\n".into()),
            ("src/theme.ts".into(), "after theme\n".into()),
        ],
    )
    .unwrap();
    assert_eq!(set.plans.len(), 2);
    assert_eq!(set.result_digest.len(), 64);
    let digest = apply_workspace_set_plan(&workspace, &set).unwrap();

    assert_eq!(digest, set.result_digest);
    assert_eq!(
        fs::read_to_string(workspace.join("src/App.tsx")).unwrap(),
        "after app\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/theme.ts")).unwrap(),
        "after theme\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn hunk_selection_reuses_the_reviewed_file_identity_and_rebinds_the_digest() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    fs::write(workspace.join("one.ts"), "one before\n").unwrap();
    fs::write(workspace.join("two.ts"), "two before\n").unwrap();
    let reviewed = prepare_workspace_apply_set(
        &workspace,
        vec![
            ("one.ts".into(), "one artifact\n".into()),
            ("two.ts".into(), "two artifact\n".into()),
        ],
    )
    .unwrap();
    let selected = select_workspace_apply_set(
        &reviewed,
        BTreeMap::from([("two.ts".into(), "two selected hunk\n".into())]),
    )
    .unwrap();

    assert_eq!(selected.plans.len(), 1);
    assert_eq!(selected.plans[0].target_path, "two.ts");
    assert_eq!(selected.plans[0].base, reviewed.plans[1].base);
    assert_ne!(selected.result_digest, reviewed.result_digest);
    apply_workspace_set_plan(&workspace, &selected).unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one before\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "two selected hunk\n"
    );
}

#[test]
fn stale_member_blocks_the_whole_set_before_any_target_is_written() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    fs::write(workspace.join("one.ts"), "one before\n").unwrap();
    fs::write(workspace.join("two.ts"), "two before\n").unwrap();
    let set = prepare_workspace_apply_set(
        &workspace,
        vec![
            ("one.ts".into(), "one after\n".into()),
            ("two.ts".into(), "two after\n".into()),
        ],
    )
    .unwrap();
    fs::write(workspace.join("two.ts"), "external writer\n").unwrap();

    assert!(matches!(
        apply_workspace_set_plan(&workspace, &set),
        Err(WorkspaceApplyError::StaleBase)
    ));
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one before\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "external writer\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn installed_members_roll_back_when_a_later_member_turns_stale() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().canonicalize().unwrap();
    fs::write(workspace.join("one.ts"), "one before\n").unwrap();
    fs::write(workspace.join("two.ts"), "two before\n").unwrap();
    let set = prepare_workspace_apply_set(
        &workspace,
        vec![
            ("one.ts".into(), "one after\n".into()),
            ("two.ts".into(), "two after\n".into()),
        ],
    )
    .unwrap();
    let mut staged = set
        .plans
        .iter()
        .map(|plan| stage_workspace_plan(&workspace, plan).unwrap())
        .collect::<Vec<_>>();
    install_transaction_plan(&mut staged[0]).unwrap();
    let replacement = workspace.join("replacement.ts");
    fs::write(&replacement, "external writer\n").unwrap();
    fs::rename(&replacement, workspace.join("two.ts")).unwrap();

    assert!(matches!(
        install_transaction_plan(&mut staged[1]),
        Err(WorkspaceApplyError::StaleBase)
    ));
    rollback_workspace_transaction(&mut staged).unwrap();
    cleanup_staged_workspace_plans(&staged);
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one before\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "external writer\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn durable_apply_keeps_a_terminal_receipt_until_the_daemon_acknowledges_it() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let state = temporary.path().join("state");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&state).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    fs::write(workspace.join("one.ts"), "one before\n").unwrap();
    fs::write(workspace.join("two.ts"), "two before\n").unwrap();
    let set = prepared_two_file_set(&workspace);
    let context = transaction_context();

    let result = apply_workspace_set_plan_durable(&workspace, &state, context, &set).unwrap();
    let DurableWorkspaceApplyResult::Committed(receipt) = result else {
        panic!("durable apply should commit");
    };
    assert_eq!(receipt.operation_id, context.operation_id);
    assert_eq!(receipt.outcome, WorkspaceTransactionOutcome::Committed);
    assert_eq!(receipt.result_digest, set.result_digest);
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one after\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "two after\n"
    );
    assert!(!contains_transaction_file(&workspace));
    let manifest = workspace_transaction_manifest_path(
        &state.join(WORKSPACE_TRANSACTION_DIRECTORY),
        receipt.transaction_id,
    );
    assert!(manifest.is_file());

    acknowledge_workspace_transaction(&state, receipt.transaction_id).unwrap();
    assert!(!manifest.exists());
}

#[test]
fn durable_apply_atomically_commits_a_write_and_a_deletion() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let state = temporary.path().join("state");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&state).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    fs::write(workspace.join("keep.ts"), "before\n").unwrap();
    fs::write(workspace.join("delete.ts"), "remove me\n").unwrap();
    let set = prepared_write_delete_set(&workspace);

    let result =
        apply_workspace_set_plan_durable(&workspace, &state, transaction_context(), &set).unwrap();
    assert!(matches!(result, DurableWorkspaceApplyResult::Committed(_)));
    assert_eq!(
        fs::read_to_string(workspace.join("keep.ts")).unwrap(),
        "after\n"
    );
    assert!(!workspace.join("delete.ts").exists());
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_rolls_back_a_partially_installed_prepared_transaction() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_test_roots(&temporary);
    let (root, manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    install_transaction_plan(&mut staged[0]).unwrap();
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::RolledBack
    );
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one before\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "two before\n"
    );
    assert!(workspace_transaction_manifest_path(&root, manifest.transaction_id).is_file());
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_commits_when_every_prepared_target_was_installed() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_test_roots(&temporary);
    let (_root, _manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    for candidate in &mut staged {
        install_transaction_plan(candidate).unwrap();
    }
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::Committed
    );
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one after\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "two after\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_restores_a_deleted_file_when_the_write_is_not_installed() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_write_delete_roots(&temporary);
    let (_root, _manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    install_transaction_plan(&mut staged[1]).unwrap();
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::RolledBack
    );
    assert_eq!(
        fs::read_to_string(workspace.join("keep.ts")).unwrap(),
        "before\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("delete.ts")).unwrap(),
        "remove me\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_commits_when_the_write_and_deletion_were_both_installed() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_write_delete_roots(&temporary);
    let (_root, _manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    for candidate in &mut staged {
        install_transaction_plan(candidate).unwrap();
    }
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::Committed
    );
    assert_eq!(
        fs::read_to_string(workspace.join("keep.ts")).unwrap(),
        "after\n"
    );
    assert!(!workspace.join("delete.ts").exists());
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_rolls_back_an_installed_binary_before_a_pending_text_write() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_binary_roots(&temporary);
    let (_root, _manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    install_transaction_plan(&mut staged[0]).unwrap();
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::RolledBack
    );
    assert_eq!(fs::read(workspace.join("image.bin")).unwrap(), [255, 0, 1]);
    assert_eq!(
        fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "before\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_commits_a_fully_installed_binary_and_text_transaction() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_binary_roots(&temporary);
    let (_root, _manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    for candidate in &mut staged {
        install_transaction_plan(candidate).unwrap();
    }
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::Committed
    );
    assert_eq!(fs::read(workspace.join("image.bin")).unwrap(), [254, 2, 3]);
    assert_eq!(
        fs::read_to_string(workspace.join("notes.txt")).unwrap(),
        "after\n"
    );
    assert!(!contains_transaction_file(&workspace));
}

#[test]
fn recovery_continues_an_interrupted_rollback() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_test_roots(&temporary);
    let (root, mut manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    install_transaction_plan(&mut staged[0]).unwrap();
    manifest.phase = WorkspaceTransactionPhase::RollingBack;
    manifest.failure_summary = Some("injected install failure".into());
    write_workspace_transaction_manifest(&root, &manifest).unwrap();
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::RolledBack
    );
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one before\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "two before\n"
    );
}

#[test]
fn recovery_cleans_an_interrupted_preparing_transaction_without_touching_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_test_roots(&temporary);
    let root = workspace_transaction_root(&state).unwrap();
    let manifest = WorkspaceTransactionManifest::new(transaction_context(), &set);
    write_workspace_transaction_manifest(&root, &manifest).unwrap();
    fs::write(
        workspace.join(&manifest.members[0].stage_name),
        "one after\n",
    )
    .unwrap();

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.blocked.is_empty());
    assert_eq!(
        report.receipts[0].outcome,
        WorkspaceTransactionOutcome::RolledBack
    );
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one before\n"
    );
    assert!(!workspace.join(&manifest.members[0].stage_name).exists());
}

#[test]
fn recovery_blocks_when_an_external_writer_changed_a_prepared_target() {
    let temporary = tempfile::tempdir().unwrap();
    let (workspace, state, set) = durable_test_roots(&temporary);
    let (root, manifest, mut staged) = stage_prepared_transaction(&workspace, &state, &set);
    install_transaction_plan(&mut staged[0]).unwrap();
    fs::write(workspace.join("two.ts"), "external writer\n").unwrap();
    drop(staged);

    let report = recover_workspace_transactions(&workspace, &state).unwrap();
    assert!(report.receipts.is_empty());
    assert_eq!(report.blocked.len(), 1);
    assert!(workspace_transaction_manifest_path(&root, manifest.transaction_id).is_file());
    assert_eq!(
        fs::read_to_string(workspace.join("one.ts")).unwrap(),
        "one after\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("two.ts")).unwrap(),
        "external writer\n"
    );
}

fn durable_test_roots(temporary: &tempfile::TempDir) -> (PathBuf, PathBuf, WorkspaceApplySetPlan) {
    let workspace = temporary.path().join("workspace");
    let state = temporary.path().join("state");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&state).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    fs::write(workspace.join("one.ts"), "one before\n").unwrap();
    fs::write(workspace.join("two.ts"), "two before\n").unwrap();
    let set = prepared_two_file_set(&workspace);
    (workspace, state, set)
}

fn prepared_two_file_set(workspace: &Path) -> WorkspaceApplySetPlan {
    prepare_workspace_apply_set(
        workspace,
        vec![
            ("one.ts".into(), "one after\n".into()),
            ("two.ts".into(), "two after\n".into()),
        ],
    )
    .unwrap()
}

fn durable_write_delete_roots(
    temporary: &tempfile::TempDir,
) -> (PathBuf, PathBuf, WorkspaceApplySetPlan) {
    let workspace = temporary.path().join("workspace");
    let state = temporary.path().join("state");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&state).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    fs::write(workspace.join("keep.ts"), "before\n").unwrap();
    fs::write(workspace.join("delete.ts"), "remove me\n").unwrap();
    let set = prepared_write_delete_set(&workspace);
    (workspace, state, set)
}

fn prepared_write_delete_set(workspace: &Path) -> WorkspaceApplySetPlan {
    prepare_workspace_apply_requests(
        workspace,
        vec![
            WorkspaceApplyRequest::Write {
                target_path: "keep.ts".into(),
                proposed_content: "after\n".into(),
            },
            WorkspaceApplyRequest::Delete {
                target_path: "delete.ts".into(),
            },
        ],
    )
    .unwrap()
}

fn durable_binary_roots(
    temporary: &tempfile::TempDir,
) -> (PathBuf, PathBuf, WorkspaceApplySetPlan) {
    let workspace = temporary.path().join("workspace");
    let state = temporary.path().join("state");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&state).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    fs::write(workspace.join("image.bin"), [255, 0, 1]).unwrap();
    fs::write(workspace.join("notes.txt"), "before\n").unwrap();
    let set = prepare_workspace_apply_requests(
        &workspace,
        vec![
            WorkspaceApplyRequest::WriteBytes {
                target_path: "image.bin".into(),
                proposed_bytes: vec![254, 2, 3],
            },
            WorkspaceApplyRequest::Write {
                target_path: "notes.txt".into(),
                proposed_content: "after\n".into(),
            },
        ],
    )
    .unwrap();
    (workspace, state, set)
}

fn transaction_context() -> WorkspaceTransactionContext {
    WorkspaceTransactionContext {
        task_id: TaskId::new(),
        operation_id: OperationId::new(),
        operation_revision: 5,
    }
}

fn stage_prepared_transaction(
    workspace: &Path,
    state: &Path,
    set: &WorkspaceApplySetPlan,
) -> (
    PathBuf,
    WorkspaceTransactionManifest,
    Vec<StagedWorkspacePlan>,
) {
    let root = workspace_transaction_root(state).unwrap();
    let mut manifest = WorkspaceTransactionManifest::new(transaction_context(), set);
    write_workspace_transaction_manifest(&root, &manifest).unwrap();
    let staged = set
        .plans
        .iter()
        .enumerate()
        .map(|(index, plan)| {
            stage_durable_workspace_plan(workspace, plan, &mut manifest.members[index]).unwrap()
        })
        .collect::<Vec<_>>();
    manifest.phase = WorkspaceTransactionPhase::Prepared;
    write_workspace_transaction_manifest(&root, &manifest).unwrap();
    (root, manifest, staged)
}

fn contains_transaction_file(root: &Path) -> bool {
    fs::read_dir(root).unwrap().any(|entry| {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            return contains_transaction_file(&entry.path());
        }
        entry
            .file_name()
            .to_string_lossy()
            .starts_with(".hyper-term-apply-")
    })
}
