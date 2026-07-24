//! Durable Tier 2 result and acceptance state.
//!
//! This module reopens only operation-bound isolated worktrees and persists
//! only reviewed workspace-apply plans. Execution and permission decisions
//! remain owned by `DaemonState`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use hyper_term_core::{OperationRecord, OperationReducer};
use hyper_term_protocol::{OperationAction, OperationId, OperationState, TaskId};
use hyper_term_sandbox::{
    IsolatedTaskReceipt, IsolatedWorktree, IsolatedWorktreeManager,
    cleanup_interrupted_lima_environment, read_isolated_task_receipt,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::workspace_apply::{WorkspaceApplySetPlan, validate_workspace_apply_set};
use crate::{
    DaemonError, ISOLATED_ACCEPTANCE_SCHEMA_VERSION, ISOLATED_TASK_CAPABILITY,
    MAX_ISOLATED_ACCEPTANCE_BYTES, cleanup_scratch_directory, is_sha256,
};

#[derive(Clone)]
pub(super) struct IsolatedResult {
    pub(super) environment: IsolatedWorktree,
    pub(super) scratch_directory: PathBuf,
    pub(super) receipt: IsolatedTaskReceipt,
}

#[derive(Clone)]
pub(super) struct IsolatedAcceptance {
    pub(super) source_operation_id: OperationId,
    pub(super) workspace: PathBuf,
    pub(super) plan: WorkspaceApplySetPlan,
    pub(super) binding_digest: String,
}

pub(super) struct PreparedIsolatedAcceptance {
    pub(super) workspace: PathBuf,
    pub(super) plan: WorkspaceApplySetPlan,
    pub(super) binding_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct StoredIsolatedAcceptance {
    pub(super) schema_version: u32,
    pub(super) acceptance_operation_id: OperationId,
    pub(super) task_id: TaskId,
    pub(super) source_operation_id: OperationId,
    pub(super) workspace: PathBuf,
    pub(super) plan: WorkspaceApplySetPlan,
    pub(super) binding_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedAcceptanceReview {
    pub operation: OperationRecord,
    pub source_operation_id: OperationId,
    pub result_digest: String,
    pub target_paths: Vec<String>,
    pub changes: Vec<IsolatedAcceptanceChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedAcceptancePreview {
    pub source_operation_id: OperationId,
    pub result_digest: String,
    pub target_paths: Vec<String>,
    pub changes: Vec<IsolatedAcceptanceChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedAcceptanceChange {
    pub target_path: String,
    pub base_digest: Option<String>,
    pub proposed_digest: String,
    pub deleted: bool,
    pub binary: bool,
    pub base_bytes: u64,
    pub proposed_bytes: u64,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedResultReview {
    pub operation_id: OperationId,
    pub receipt: IsolatedTaskReceipt,
}

pub(super) fn recover_completed_isolated_results(
    manager: &IsolatedWorktreeManager,
    root: &Path,
    operations: &OperationReducer,
) -> Result<HashMap<OperationId, IsolatedResult>, DaemonError> {
    let mut recovered = HashMap::new();
    for operation_entry in fs::read_dir(root)?.take(1_025) {
        let operation_entry = operation_entry?;
        let metadata = operation_entry.file_type()?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(DaemonError::InvalidIsolatedResultStore);
        }
        if recovered.len() == 1_024 {
            return Err(DaemonError::InvalidIsolatedResultStore);
        }
        let operation_text = operation_entry
            .file_name()
            .into_string()
            .map_err(|_| DaemonError::InvalidIsolatedResultStore)?;
        let operation_id = OperationId::from(
            Uuid::parse_str(&operation_text)
                .map_err(|_| DaemonError::InvalidIsolatedResultStore)?,
        );
        let operation = operations
            .records()
            .find(|record| record.operation_id == operation_id)
            .ok_or(DaemonError::InvalidIsolatedResultStore)?;
        if !operation
            .required_capabilities
            .iter()
            .any(|capability| capability == ISOLATED_TASK_CAPABILITY)
        {
            return Err(DaemonError::InvalidIsolatedResultStore);
        }
        let mut completed = None;
        let mut removed_interrupted = false;
        for environment_entry in fs::read_dir(operation_entry.path())?.take(3) {
            let environment_entry = environment_entry?;
            if !environment_entry.file_type()?.is_dir() {
                return Err(DaemonError::InvalidIsolatedResultStore);
            }
            let environment = manager.reopen(environment_entry.path())?;
            if environment.manifest.task_id != operation_id.to_string() {
                return Err(DaemonError::InvalidIsolatedResultStore);
            }
            if !environment
                .environment_root
                .join("task-receipt.json")
                .is_file()
            {
                cleanup_interrupted_lima_environment(&environment.manifest.environment_id)?;
                manager.destroy(&environment)?;
                removed_interrupted = true;
                continue;
            }
            if completed.is_some() {
                return Err(DaemonError::InvalidIsolatedResultStore);
            }
            let receipt = read_isolated_task_receipt(&environment)?;
            let live_changes = match manager.inspect_changes(&environment) {
                Ok(changes) => changes,
                Err(error) => {
                    // The source repository may have been a temporary
                    // workspace that no longer exists after a restart. The
                    // retained result is not safe to review or accept without
                    // a live Git binding, but it must not prevent the daemon
                    // (and therefore the whole desktop app) from starting.
                    // Keep the durable directory in place so a restored
                    // source workspace can be validated on a later launch.
                    eprintln!(
                        "hyper-term: Tier 2 result {operation_id} is unavailable and remains preserved at {}: {error}",
                        environment.environment_root.display()
                    );
                    continue;
                }
            };
            if live_changes != receipt.changes {
                return Err(DaemonError::IsolatedResultDigestMismatch);
            }
            completed = Some(IsolatedResult {
                environment,
                scratch_directory: operation_entry.path(),
                receipt,
            });
        }
        if let Some(result) = completed {
            recovered.insert(operation_id, result);
        } else if removed_interrupted {
            cleanup_scratch_directory(&operation_entry.path());
        }
    }
    Ok(recovered)
}

pub(super) fn recover_isolated_acceptances(
    root: &Path,
    operations: &OperationReducer,
    results: &HashMap<OperationId, IsolatedResult>,
) -> Result<HashMap<OperationId, IsolatedAcceptance>, DaemonError> {
    let mut recovered = HashMap::new();
    let mut recovered_sources = HashSet::new();
    for entry in fs::read_dir(root)?.take(1_025) {
        let entry = entry?;
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
        if file_name.starts_with('.') && file_name.ends_with(".tmp") {
            if !entry.file_type()?.is_file() {
                return Err(DaemonError::InvalidIsolatedAcceptanceStore);
            }
            fs::remove_file(entry.path())?;
            continue;
        }
        if recovered.len() == 1_024 || !entry.file_type()?.is_file() {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        let operation_text = file_name
            .strip_suffix(".json")
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        let operation_id = OperationId::from(
            Uuid::parse_str(operation_text)
                .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?,
        );
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_ISOLATED_ACCEPTANCE_BYTES
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        let bytes = fs::read(entry.path())?;
        let stored: StoredIsolatedAcceptance = serde_json::from_slice(&bytes)
            .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
        if stored.schema_version != ISOLATED_ACCEPTANCE_SCHEMA_VERSION
            || stored.acceptance_operation_id != operation_id
            || !is_sha256(&stored.binding_digest)
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        validate_workspace_apply_set(&stored.plan)
            .map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
        let operation = operations
            .records()
            .find(|record| record.operation_id == operation_id)
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        if operation.task_id != stored.task_id
            || !matches!(
                &operation.action,
                OperationAction::Opaque { kind, payload_digest }
                    if kind == "hyper_term.tier2.accept"
                        && payload_digest == &stored.binding_digest
            )
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        let source_operation = operations
            .records()
            .find(|record| record.operation_id == stored.source_operation_id)
            .ok_or(DaemonError::InvalidIsolatedAcceptanceStore)?;
        let Some(result) = results.get(&stored.source_operation_id) else {
            // A result whose Git binding is temporarily unavailable is not
            // recovered above. Preserve its reviewed acceptance too, but do
            // not expose either object as executable state until the result
            // can be validated again.
            eprintln!(
                "hyper-term: Tier 2 acceptance {operation_id} is unavailable because result {} could not be recovered; preserving {}",
                stored.source_operation_id,
                entry.path().display()
            );
            continue;
        };
        if source_operation.task_id != stored.task_id
            || result.environment.manifest.source_workspace != stored.workspace
            || isolated_acceptance_digest(
                stored.source_operation_id,
                &result.receipt.changes.inventory_sha256,
                &stored.workspace,
                &stored.plan,
            )? != stored.binding_digest
            || stored.plan.plans.iter().any(|plan| {
                !result.receipt.changes.changed_files.iter().any(|change| {
                    change.path == Path::new(&plan.target_path)
                        && if plan.deletes_target() {
                            change.kind == hyper_term_sandbox::IsolatedChangeKind::Deleted
                                && change.content_sha256.is_none()
                        } else {
                            change.content_sha256.as_deref() == Some(&plan.proposed_digest)
                        }
                })
            })
        {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        if !matches!(
            operation.state,
            OperationState::WaitingHuman
                | OperationState::Authorized
                | OperationState::Dispatching
                | OperationState::UnknownExecution
        ) {
            fs::remove_file(entry.path())?;
            continue;
        }
        if !recovered_sources.insert(stored.source_operation_id) {
            return Err(DaemonError::InvalidIsolatedAcceptanceStore);
        }
        recovered.insert(
            operation_id,
            IsolatedAcceptance {
                source_operation_id: stored.source_operation_id,
                workspace: stored.workspace,
                plan: stored.plan,
                binding_digest: stored.binding_digest,
            },
        );
    }
    acceptance_root_file(root)?.sync_all()?;
    Ok(recovered)
}

fn isolated_acceptance_path(root: &Path, operation_id: OperationId) -> PathBuf {
    root.join(format!("{operation_id}.json"))
}

fn acceptance_root_file(root: &Path) -> Result<File, DaemonError> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?)
}

pub(super) fn write_isolated_acceptance(
    root: &Path,
    stored: &StoredIsolatedAcceptance,
) -> Result<(), DaemonError> {
    let bytes =
        serde_json::to_vec(stored).map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
    if bytes.len() as u64 > MAX_ISOLATED_ACCEPTANCE_BYTES {
        return Err(DaemonError::InvalidIsolatedAcceptanceStore);
    }
    let target = isolated_acceptance_path(root, stored.acceptance_operation_id);
    if target.exists() {
        return Err(DaemonError::InvalidIsolatedAcceptanceStore);
    }
    let temporary = root.join(format!(
        ".{}.{}.tmp",
        stored.acceptance_operation_id,
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        acceptance_root_file(root)?.sync_all()?;
        Ok::<(), DaemonError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub(super) fn remove_isolated_acceptance(
    root: &Path,
    operation_id: OperationId,
) -> Result<(), DaemonError> {
    match fs::remove_file(isolated_acceptance_path(root, operation_id)) {
        Ok(()) => acceptance_root_file(root)?.sync_all().map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn safe_isolated_result_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(name) if name != ".git"))
}

pub(super) fn isolated_acceptance_changes(
    plan: &WorkspaceApplySetPlan,
) -> Vec<IsolatedAcceptanceChange> {
    plan.plans
        .iter()
        .map(|plan| IsolatedAcceptanceChange {
            target_path: plan.target_path.clone(),
            base_digest: plan.base_digest().map(str::to_owned),
            proposed_digest: plan.proposed_digest.clone(),
            deleted: plan.deletes_target(),
            binary: plan.is_binary(),
            base_bytes: plan.base_bytes_len(),
            proposed_bytes: plan.proposed_bytes_len(),
            before: plan.base_content().to_owned(),
            after: plan.proposed_content.clone(),
        })
        .collect()
}

pub(super) fn isolated_acceptance_digest(
    source_operation_id: OperationId,
    inventory_sha256: &str,
    workspace: &Path,
    plan: &WorkspaceApplySetPlan,
) -> Result<String, DaemonError> {
    let plan = serde_json::to_vec(plan).map_err(|_| DaemonError::InvalidIsolatedAcceptanceStore)?;
    let mut digest = Sha256::new();
    digest.update(b"hyper-term-tier2-acceptance-v2\0");
    digest.update(source_operation_id.to_string().as_bytes());
    digest.update([0]);
    digest.update(inventory_sha256.as_bytes());
    digest.update([0]);
    digest.update(workspace.as_os_str().as_bytes());
    digest.update([0]);
    digest.update(plan);
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) fn isolated_acceptance_summary(
    source_operation_id: OperationId,
    target_paths: &[String],
) -> String {
    const MAX_PATH_PREVIEW_BYTES: usize = 320;
    let mut preview = String::new();
    for path in target_paths {
        let separator_bytes = usize::from(!preview.is_empty()) * 2;
        if preview.len() + separator_bytes + path.len() > MAX_PATH_PREVIEW_BYTES {
            if !preview.is_empty() {
                preview.push_str(", ");
            }
            preview.push_str("more files");
            break;
        }
        if !preview.is_empty() {
            preview.push_str(", ");
        }
        preview.push_str(path);
    }
    format!(
        "Apply {} reviewed Tier 2 file(s) from operation {source_operation_id}: {preview}",
        target_paths.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_result_paths_are_relative_normal_workspace_paths() {
        assert!(safe_isolated_result_path(Path::new("src/main.rs")));
        assert!(!safe_isolated_result_path(Path::new("")));
        assert!(!safe_isolated_result_path(Path::new("/tmp/result")));
        assert!(!safe_isolated_result_path(Path::new("../outside")));
        assert!(!safe_isolated_result_path(Path::new(".git/config")));
    }

    #[test]
    fn acceptance_summary_is_bounded_and_preserves_operation_identity() {
        let operation_id = OperationId::from(Uuid::nil());
        let paths = vec!["a".repeat(200), "b".repeat(200)];
        let summary = isolated_acceptance_summary(operation_id, &paths);
        assert!(summary.contains(&operation_id.to_string()));
        assert!(summary.contains("more files"));
        assert!(summary.len() < 512);
    }
}
