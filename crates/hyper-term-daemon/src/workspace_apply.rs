use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use hyper_term_protocol::{OperationId, TaskId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

mod workspace_io;
use workspace_io::*;

pub(crate) const MAX_WORKSPACE_FILE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_WORKSPACE_APPLY_FILES: usize = 32;
pub(crate) const MAX_WORKSPACE_APPLY_BYTES: usize = 4 * 1024 * 1024;
const MAX_TARGET_PATH_BYTES: usize = 4096;
const WORKSPACE_TRANSACTION_SCHEMA_VERSION: u32 = 1;
const MAX_WORKSPACE_TRANSACTION_MANIFEST_BYTES: u64 = 256 * 1024;
const WORKSPACE_TRANSACTION_DIRECTORY: &str = "workspace-transactions";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceFileSnapshot {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binary_bytes: Option<u64>,
    pub digest: String,
    device: u64,
    inode: u64,
    mode: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceApplyPlan {
    pub target_path: String,
    pub proposed_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposed_binary_base64: Option<String>,
    pub proposed_digest: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub delete: bool,
    pub base: Option<WorkspaceFileSnapshot>,
    parent_device: u64,
    parent_inode: u64,
}

impl WorkspaceApplyPlan {
    pub(crate) fn base_content(&self) -> &str {
        self.base
            .as_ref()
            .map(|snapshot| snapshot.content.as_str())
            .unwrap_or_default()
    }

    pub(crate) fn base_digest(&self) -> Option<&str> {
        self.base.as_ref().map(|snapshot| snapshot.digest.as_str())
    }

    pub(crate) fn proposed_bytes(&self) -> Result<Cow<'_, [u8]>, WorkspaceApplyError> {
        match self.proposed_binary_base64.as_deref() {
            Some(encoded) => BASE64_STANDARD
                .decode(encoded)
                .map(Cow::Owned)
                .map_err(|_| WorkspaceApplyError::InvalidPath),
            None => Ok(Cow::Borrowed(self.proposed_content.as_bytes())),
        }
    }

    pub(crate) fn base_bytes_len(&self) -> u64 {
        self.base
            .as_ref()
            .map(WorkspaceFileSnapshot::bytes_len)
            .unwrap_or_default()
    }

    pub(crate) fn proposed_bytes_len(&self) -> u64 {
        self.proposed_binary_base64
            .as_ref()
            .and_then(|encoded| decoded_base64_len(encoded).ok())
            .map(|length| length as u64)
            .unwrap_or(self.proposed_content.len() as u64)
    }

    pub(crate) fn is_binary(&self) -> bool {
        self.proposed_binary_base64.is_some()
            || self
                .base
                .as_ref()
                .is_some_and(WorkspaceFileSnapshot::is_binary)
    }

    pub(crate) fn deletes_target(&self) -> bool {
        self.delete
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceApplyRequest {
    Write {
        target_path: String,
        proposed_content: String,
    },
    WriteBytes {
        target_path: String,
        proposed_bytes: Vec<u8>,
    },
    Delete {
        target_path: String,
    },
}

impl WorkspaceFileSnapshot {
    fn bytes_len(&self) -> u64 {
        self.binary_bytes.unwrap_or(self.content.len() as u64)
    }

    fn is_binary(&self) -> bool {
        self.binary_bytes.is_some()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceApplySetPlan {
    pub plans: Vec<WorkspaceApplyPlan>,
    pub result_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceTransactionContext {
    pub task_id: TaskId,
    pub operation_id: OperationId,
    pub operation_revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkspaceTransactionPhase {
    Preparing,
    Prepared,
    RollingBack,
    Committed,
    RolledBack,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceTransactionOutcome {
    Committed,
    RolledBack,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceTransactionReceipt {
    pub transaction_id: Uuid,
    pub task_id: TaskId,
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub result_digest: String,
    pub outcome: WorkspaceTransactionOutcome,
    pub failure_summary: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DurableWorkspaceApplyResult {
    Committed(WorkspaceTransactionReceipt),
    RolledBack(WorkspaceTransactionReceipt),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct WorkspaceRecoveryReport {
    pub receipts: Vec<WorkspaceTransactionReceipt>,
    pub blocked: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WorkspaceRecoveryIdentity {
    digest: String,
    device: u64,
    inode: u64,
    mode: u32,
}

impl WorkspaceRecoveryIdentity {
    fn from_snapshot(snapshot: &WorkspaceFileSnapshot) -> Self {
        Self {
            digest: snapshot.digest.clone(),
            device: snapshot.device,
            inode: snapshot.inode,
            mode: snapshot.mode,
        }
    }

    fn matches(&self, snapshot: &WorkspaceFileSnapshot) -> bool {
        self.digest == snapshot.digest
            && self.device == snapshot.device
            && self.inode == snapshot.inode
            && self.mode == snapshot.mode
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WorkspaceTransactionMember {
    target_path: String,
    parent_device: u64,
    parent_inode: u64,
    stage_name: String,
    backup_name: Option<String>,
    base: Option<WorkspaceRecoveryIdentity>,
    proposed_digest: String,
    proposed_mode: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    delete: bool,
    staged: Option<WorkspaceRecoveryIdentity>,
    backup: Option<WorkspaceRecoveryIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WorkspaceTransactionManifest {
    schema_version: u32,
    transaction_id: Uuid,
    task_id: TaskId,
    operation_id: OperationId,
    operation_revision: u64,
    result_digest: String,
    phase: WorkspaceTransactionPhase,
    failure_summary: Option<String>,
    members: Vec<WorkspaceTransactionMember>,
}

#[derive(Debug, Error)]
pub(crate) enum WorkspaceApplyError {
    #[error("workspace target path is invalid")]
    InvalidPath,
    #[error("workspace target parent is unavailable")]
    ParentUnavailable,
    #[error("workspace target parent changed after review")]
    ParentChanged,
    #[error("workspace target is not a bounded regular UTF-8 file")]
    UnsupportedTarget,
    #[error("workspace target exceeds the bounded file size")]
    TooLarge,
    #[error("workspace target changed after review")]
    StaleBase,
    #[error("workspace apply may have executed but could not be verified: {0}")]
    UnknownExecution(String),
    #[error("workspace transaction recovery is required: {0}")]
    RecoveryRequired(String),
    #[error("workspace target already matches the artifact")]
    NoChanges,
    #[error("workspace apply I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

struct OpenParent {
    directory: File,
    file_name: CString,
    device: u64,
    inode: u64,
}

#[cfg(test)]
pub(crate) fn prepare_workspace_apply(
    workspace: &Path,
    target_path: &str,
    proposed_content: String,
) -> Result<WorkspaceApplyPlan, WorkspaceApplyError> {
    let mut set =
        prepare_workspace_apply_set(workspace, vec![(target_path.to_owned(), proposed_content)])?;
    Ok(set
        .plans
        .pop()
        .expect("a single-file set always retains its changed plan"))
}

pub(crate) fn prepare_workspace_apply_set(
    workspace: &Path,
    requests: Vec<(String, String)>,
) -> Result<WorkspaceApplySetPlan, WorkspaceApplyError> {
    prepare_workspace_apply_requests(
        workspace,
        requests
            .into_iter()
            .map(
                |(target_path, proposed_content)| WorkspaceApplyRequest::Write {
                    target_path,
                    proposed_content,
                },
            )
            .collect(),
    )
}

pub(crate) fn prepare_workspace_apply_requests(
    workspace: &Path,
    requests: Vec<WorkspaceApplyRequest>,
) -> Result<WorkspaceApplySetPlan, WorkspaceApplyError> {
    if requests.is_empty() || requests.len() > MAX_WORKSPACE_APPLY_FILES {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    let total_bytes = requests
        .iter()
        .try_fold(0_usize, |total, request| {
            let bytes = match request {
                WorkspaceApplyRequest::Write {
                    proposed_content, ..
                } => proposed_content.len(),
                WorkspaceApplyRequest::WriteBytes { proposed_bytes, .. } => proposed_bytes.len(),
                WorkspaceApplyRequest::Delete { .. } => 0,
            };
            total.checked_add(bytes)
        })
        .ok_or(WorkspaceApplyError::TooLarge)?;
    if total_bytes > MAX_WORKSPACE_APPLY_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let mut targets = BTreeSet::new();
    let mut plans = Vec::with_capacity(requests.len());
    for request in requests {
        let target_path = match &request {
            WorkspaceApplyRequest::Write { target_path, .. }
            | WorkspaceApplyRequest::WriteBytes { target_path, .. }
            | WorkspaceApplyRequest::Delete { target_path } => target_path,
        };
        if !targets.insert(target_path.clone()) {
            return Err(WorkspaceApplyError::InvalidPath);
        }
        let prepared = match request {
            WorkspaceApplyRequest::Write {
                target_path,
                proposed_content,
            } => prepare_workspace_file(workspace, &target_path, proposed_content),
            WorkspaceApplyRequest::WriteBytes {
                target_path,
                proposed_bytes,
            } => prepare_workspace_bytes(workspace, &target_path, proposed_bytes),
            WorkspaceApplyRequest::Delete { target_path } => {
                prepare_workspace_deletion(workspace, &target_path)
            }
        };
        match prepared {
            Ok(plan) => plans.push(plan),
            Err(WorkspaceApplyError::NoChanges) => {}
            Err(error) => return Err(error),
        }
    }
    if plans.is_empty() {
        return Err(WorkspaceApplyError::NoChanges);
    }
    let base_bytes = plans.iter().try_fold(0_usize, |total, plan| {
        total.checked_add(plan.base_bytes_len() as usize)
    });
    if base_bytes.is_none_or(|bytes| bytes > MAX_WORKSPACE_APPLY_BYTES) {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let result_digest = workspace_set_digest(&plans);
    Ok(WorkspaceApplySetPlan {
        plans,
        result_digest,
    })
}

fn prepare_workspace_file(
    workspace: &Path,
    target_path: &str,
    proposed_content: String,
) -> Result<WorkspaceApplyPlan, WorkspaceApplyError> {
    prepare_workspace_bytes(workspace, target_path, proposed_content.into_bytes())
}

fn prepare_workspace_bytes(
    workspace: &Path,
    target_path: &str,
    proposed_bytes: Vec<u8>,
) -> Result<WorkspaceApplyPlan, WorkspaceApplyError> {
    if proposed_bytes.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let relative = validate_target_path(target_path)?;
    let parent = open_parent(workspace, &relative)?;
    let base = read_target_at(parent.directory.as_raw_fd(), &parent.file_name)?;
    if base
        .as_ref()
        .is_some_and(|snapshot| snapshot.digest == sha256_bytes(&proposed_bytes))
    {
        return Err(WorkspaceApplyError::NoChanges);
    }
    let (proposed_content, proposed_binary_base64) = encode_workspace_content(&proposed_bytes);
    Ok(WorkspaceApplyPlan {
        target_path: target_path.to_owned(),
        proposed_digest: sha256_bytes(&proposed_bytes),
        proposed_content,
        proposed_binary_base64,
        delete: false,
        base,
        parent_device: parent.device,
        parent_inode: parent.inode,
    })
}

fn prepare_workspace_deletion(
    workspace: &Path,
    target_path: &str,
) -> Result<WorkspaceApplyPlan, WorkspaceApplyError> {
    let relative = validate_target_path(target_path)?;
    let parent = open_parent(workspace, &relative)?;
    let base = read_target_at(parent.directory.as_raw_fd(), &parent.file_name)?
        .ok_or(WorkspaceApplyError::NoChanges)?;
    Ok(WorkspaceApplyPlan {
        target_path: target_path.to_owned(),
        proposed_content: String::new(),
        proposed_binary_base64: None,
        proposed_digest: workspace_deletion_digest(),
        delete: true,
        base: Some(base),
        parent_device: parent.device,
        parent_inode: parent.inode,
    })
}

#[cfg(test)]
pub(crate) fn apply_workspace_plan(
    workspace: &Path,
    plan: &WorkspaceApplyPlan,
) -> Result<String, WorkspaceApplyError> {
    let set = WorkspaceApplySetPlan {
        plans: vec![plan.clone()],
        result_digest: workspace_set_digest(std::slice::from_ref(plan)),
    };
    apply_workspace_set_plan(workspace, &set)
}

#[cfg(test)]
pub(crate) fn apply_workspace_set_plan(
    workspace: &Path,
    set: &WorkspaceApplySetPlan,
) -> Result<String, WorkspaceApplyError> {
    validate_workspace_set(set)?;
    if set.plans.len() == 1 {
        apply_single_workspace_plan(workspace, &set.plans[0])?;
        return Ok(set.result_digest.clone());
    }
    apply_workspace_transaction(workspace, set)?;
    Ok(set.result_digest.clone())
}

pub(crate) fn select_workspace_apply_set(
    reviewed: &WorkspaceApplySetPlan,
    selections: BTreeMap<String, String>,
) -> Result<WorkspaceApplySetPlan, WorkspaceApplyError> {
    if selections.is_empty() || selections.len() > reviewed.plans.len() {
        return Err(WorkspaceApplyError::NoChanges);
    }
    if selections.keys().any(|target| {
        !reviewed
            .plans
            .iter()
            .any(|plan| &plan.target_path == target)
    }) {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    let mut total_bytes = 0_usize;
    let mut plans = Vec::with_capacity(selections.len());
    for reviewed_plan in &reviewed.plans {
        let Some(content) = selections.get(&reviewed_plan.target_path) else {
            continue;
        };
        if reviewed_plan.delete || reviewed_plan.is_binary() {
            return Err(WorkspaceApplyError::InvalidPath);
        }
        total_bytes = total_bytes
            .checked_add(content.len())
            .ok_or(WorkspaceApplyError::TooLarge)?;
        if content.len() > MAX_WORKSPACE_FILE_BYTES || total_bytes > MAX_WORKSPACE_APPLY_BYTES {
            return Err(WorkspaceApplyError::TooLarge);
        }
        if content == reviewed_plan.base_content() {
            return Err(WorkspaceApplyError::NoChanges);
        }
        let mut plan = reviewed_plan.clone();
        plan.proposed_digest = sha256_bytes(content.as_bytes());
        plan.proposed_content = content.clone();
        plan.proposed_binary_base64 = None;
        plans.push(plan);
    }
    if plans.is_empty() {
        return Err(WorkspaceApplyError::NoChanges);
    }
    let result_digest = workspace_set_digest(&plans);
    Ok(WorkspaceApplySetPlan {
        plans,
        result_digest,
    })
}

pub(crate) fn apply_workspace_set_plan_durable(
    workspace: &Path,
    state_directory: &Path,
    context: WorkspaceTransactionContext,
    set: &WorkspaceApplySetPlan,
) -> Result<DurableWorkspaceApplyResult, WorkspaceApplyError> {
    validate_workspace_set(set)?;
    let transaction_root = workspace_transaction_root(state_directory)?;
    let mut manifest = WorkspaceTransactionManifest::new(context, set);
    write_workspace_transaction_manifest(&transaction_root, &manifest)?;

    let mut staged = Vec::with_capacity(set.plans.len());
    for (index, plan) in set.plans.iter().enumerate() {
        match stage_durable_workspace_plan(workspace, plan, &mut manifest.members[index]) {
            Ok(candidate) => staged.push(candidate),
            Err(error) => {
                cleanup_manifest_files(workspace, &manifest)?;
                for member in &mut manifest.members {
                    member.staged = None;
                    member.backup = None;
                }
                manifest.phase = WorkspaceTransactionPhase::RolledBack;
                manifest.failure_summary = Some(error.to_string());
                write_workspace_transaction_manifest(&transaction_root, &manifest)?;
                return Ok(DurableWorkspaceApplyResult::RolledBack(manifest.receipt()));
            }
        }
    }

    manifest.phase = WorkspaceTransactionPhase::Prepared;
    write_workspace_transaction_manifest(&transaction_root, &manifest)?;
    for (index, staged_plan) in staged.iter_mut().enumerate() {
        if !staged_plan.plan.delete {
            let staged_snapshot = read_target_at(
                staged_plan.parent.directory.as_raw_fd(),
                &staged_plan.stage_name,
            )?
            .ok_or_else(|| {
                WorkspaceApplyError::RecoveryRequired("prepared stage disappeared".into())
            })?;
            if !manifest.members[index]
                .staged
                .as_ref()
                .is_some_and(|identity| identity.matches(&staged_snapshot))
            {
                return Err(WorkspaceApplyError::RecoveryRequired(
                    "prepared stage identity changed".into(),
                ));
            }
        }
        if let Err(error) = install_transaction_plan(staged_plan) {
            manifest.phase = WorkspaceTransactionPhase::RollingBack;
            manifest.failure_summary = Some(error.to_string());
            write_workspace_transaction_manifest(&transaction_root, &manifest).map_err(
                |write| {
                    WorkspaceApplyError::UnknownExecution(format!(
                        "{error}; could not persist rollback intent: {write}"
                    ))
                },
            )?;
            rollback_workspace_manifest(workspace, &manifest).map_err(|rollback| {
                WorkspaceApplyError::UnknownExecution(format!(
                    "{error}; rollback could not be verified: {rollback}"
                ))
            })?;
            manifest.phase = WorkspaceTransactionPhase::RolledBack;
            write_workspace_transaction_manifest(&transaction_root, &manifest)?;
            cleanup_manifest_files(workspace, &manifest)?;
            return Ok(DurableWorkspaceApplyResult::RolledBack(manifest.receipt()));
        }
    }

    manifest.phase = WorkspaceTransactionPhase::Committed;
    write_workspace_transaction_manifest(&transaction_root, &manifest).map_err(|error| {
        WorkspaceApplyError::UnknownExecution(format!(
            "workspace targets were installed but commit could not be persisted: {error}"
        ))
    })?;
    cleanup_manifest_files(workspace, &manifest).map_err(|error| {
        WorkspaceApplyError::UnknownExecution(format!(
            "workspace transaction committed but cleanup failed: {error}"
        ))
    })?;
    Ok(DurableWorkspaceApplyResult::Committed(manifest.receipt()))
}

pub(crate) fn recover_workspace_transactions(
    workspace: &Path,
    state_directory: &Path,
) -> Result<WorkspaceRecoveryReport, WorkspaceApplyError> {
    let transaction_root = workspace_transaction_root(state_directory)?;
    let mut entries = fs::read_dir(&transaction_root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut report = WorkspaceRecoveryReport::default();
    for entry in entries {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            report
                .blocked
                .push("workspace transaction journal contains a non-UTF-8 entry".into());
            continue;
        };
        if file_name.starts_with('.') && file_name.ends_with(".tmp") {
            if let Err(error) = fs::remove_file(entry.path())
                && error.kind() != std::io::ErrorKind::NotFound
            {
                report.blocked.push(format!(
                    "workspace transaction temporary manifest {file_name} could not be removed"
                ));
            }
            continue;
        }
        let Some(id_text) = file_name.strip_suffix(".json") else {
            report.blocked.push(format!(
                "workspace transaction journal contains unexpected entry {file_name}"
            ));
            continue;
        };
        let Ok(transaction_id) = Uuid::parse_str(id_text) else {
            report.blocked.push(format!(
                "workspace transaction manifest name is invalid: {file_name}"
            ));
            continue;
        };
        let mut manifest = match read_workspace_transaction_manifest(&entry.path()) {
            Ok(manifest) if manifest.transaction_id == transaction_id => manifest,
            Ok(_) => {
                report.blocked.push(format!(
                    "workspace transaction manifest id does not match {file_name}"
                ));
                continue;
            }
            Err(error) => {
                report.blocked.push(format!(
                    "workspace transaction manifest {file_name} is unreadable: {error}"
                ));
                continue;
            }
        };
        match recover_workspace_transaction(workspace, &transaction_root, &mut manifest) {
            Ok(receipt) => report.receipts.push(receipt),
            Err(error) => report.blocked.push(format!(
                "workspace transaction {} requires manual recovery: {error}",
                manifest.transaction_id
            )),
        }
    }
    transaction_root_file(&transaction_root)?.sync_all()?;
    Ok(report)
}

pub(crate) fn acknowledge_workspace_transaction(
    state_directory: &Path,
    transaction_id: Uuid,
) -> Result<(), WorkspaceApplyError> {
    let transaction_root = workspace_transaction_root(state_directory)?;
    let path = workspace_transaction_manifest_path(&transaction_root, transaction_id);
    match fs::remove_file(path) {
        Ok(()) => transaction_root_file(&transaction_root)?
            .sync_all()
            .map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

impl WorkspaceTransactionManifest {
    fn new(context: WorkspaceTransactionContext, set: &WorkspaceApplySetPlan) -> Self {
        let transaction_id = Uuid::new_v4();
        let members = set
            .plans
            .iter()
            .enumerate()
            .map(|(index, plan)| WorkspaceTransactionMember {
                target_path: plan.target_path.clone(),
                parent_device: plan.parent_device,
                parent_inode: plan.parent_inode,
                stage_name: workspace_stage_name(transaction_id, index),
                backup_name: plan
                    .base
                    .as_ref()
                    .map(|_| workspace_backup_name(transaction_id, index)),
                base: plan
                    .base
                    .as_ref()
                    .map(WorkspaceRecoveryIdentity::from_snapshot),
                proposed_digest: plan.proposed_digest.clone(),
                proposed_mode: plan
                    .base
                    .as_ref()
                    .map(|base| base.mode & 0o7777)
                    .unwrap_or(0o644),
                delete: plan.delete,
                staged: None,
                backup: None,
            })
            .collect();
        Self {
            schema_version: WORKSPACE_TRANSACTION_SCHEMA_VERSION,
            transaction_id,
            task_id: context.task_id,
            operation_id: context.operation_id,
            operation_revision: context.operation_revision,
            result_digest: set.result_digest.clone(),
            phase: WorkspaceTransactionPhase::Preparing,
            failure_summary: None,
            members,
        }
    }

    fn receipt(&self) -> WorkspaceTransactionReceipt {
        let outcome = match self.phase {
            WorkspaceTransactionPhase::Committed => WorkspaceTransactionOutcome::Committed,
            WorkspaceTransactionPhase::RolledBack => WorkspaceTransactionOutcome::RolledBack,
            _ => unreachable!("only terminal transaction manifests yield receipts"),
        };
        WorkspaceTransactionReceipt {
            transaction_id: self.transaction_id,
            task_id: self.task_id,
            operation_id: self.operation_id,
            operation_revision: self.operation_revision,
            result_digest: self.result_digest.clone(),
            outcome,
            failure_summary: self.failure_summary.clone(),
        }
    }
}

pub(crate) fn validate_workspace_apply_set(
    set: &WorkspaceApplySetPlan,
) -> Result<(), WorkspaceApplyError> {
    if set.plans.is_empty() || set.plans.len() > MAX_WORKSPACE_APPLY_FILES {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    let mut targets = BTreeSet::new();
    let mut proposed_bytes = 0_usize;
    let mut base_bytes = 0_usize;
    for plan in &set.plans {
        validate_target_path(&plan.target_path)?;
        let proposed = plan.proposed_bytes()?;
        let invalid_proposal = if plan.delete {
            plan.base.is_none()
                || !plan.proposed_content.is_empty()
                || plan.proposed_binary_base64.is_some()
                || plan.proposed_digest != workspace_deletion_digest()
        } else {
            proposed.len() > MAX_WORKSPACE_FILE_BYTES
                || sha256_bytes(proposed.as_ref()) != plan.proposed_digest
                || plan.proposed_binary_base64.is_some()
                    && (proposed.is_empty()
                        || !plan.proposed_content.is_empty()
                        || std::str::from_utf8(proposed.as_ref()).is_ok()
                        || plan.proposed_binary_base64.as_deref()
                            != Some(BASE64_STANDARD.encode(proposed.as_ref()).as_str()))
        };
        if !targets.insert(&plan.target_path)
            || invalid_proposal
            || plan.base.as_ref().is_some_and(|base| {
                base.bytes_len() > MAX_WORKSPACE_FILE_BYTES as u64
                    || !is_sha256(&base.digest)
                    || base.binary_bytes.is_some()
                        && (base.binary_bytes == Some(0) || !base.content.is_empty())
                    || base.binary_bytes.is_none()
                        && sha256_bytes(base.content.as_bytes()) != base.digest
                    || (!plan.delete && base.digest == plan.proposed_digest)
            })
        {
            return Err(WorkspaceApplyError::InvalidPath);
        }
        proposed_bytes = proposed_bytes
            .checked_add(proposed.len())
            .ok_or(WorkspaceApplyError::TooLarge)?;
        base_bytes = base_bytes
            .checked_add(plan.base_bytes_len() as usize)
            .ok_or(WorkspaceApplyError::TooLarge)?;
    }
    if proposed_bytes > MAX_WORKSPACE_APPLY_BYTES
        || base_bytes > MAX_WORKSPACE_APPLY_BYTES
        || workspace_set_digest(&set.plans) != set.result_digest
    {
        return Err(WorkspaceApplyError::TooLarge);
    }
    Ok(())
}

fn validate_workspace_set(set: &WorkspaceApplySetPlan) -> Result<(), WorkspaceApplyError> {
    validate_workspace_apply_set(set)
}

fn workspace_transaction_root(state_directory: &Path) -> Result<PathBuf, WorkspaceApplyError> {
    let root = state_directory.join(WORKSPACE_TRANSACTION_DIRECTORY);
    if root.exists() {
        let metadata = fs::symlink_metadata(&root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkspaceApplyError::RecoveryRequired(
                "transaction journal root is not a private directory".into(),
            ));
        }
    } else {
        fs::create_dir(&root)?;
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    transaction_root_file(&root)?.sync_all()?;
    Ok(root)
}

fn transaction_root_file(root: &Path) -> Result<File, WorkspaceApplyError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(Into::into)
}

fn workspace_transaction_manifest_path(root: &Path, transaction_id: Uuid) -> PathBuf {
    root.join(format!("{transaction_id}.json"))
}

fn write_workspace_transaction_manifest(
    root: &Path,
    manifest: &WorkspaceTransactionManifest,
) -> Result<(), WorkspaceApplyError> {
    validate_workspace_transaction_manifest(manifest)?;
    let bytes = serde_json::to_vec(manifest).map_err(|error| {
        WorkspaceApplyError::RecoveryRequired(format!(
            "transaction manifest could not be serialized: {error}"
        ))
    })?;
    if bytes.len() as u64 > MAX_WORKSPACE_TRANSACTION_MANIFEST_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let temporary = root.join(format!(
        ".{}.{}.tmp",
        manifest.transaction_id,
        Uuid::new_v4()
    ));
    let target = workspace_transaction_manifest_path(root, manifest.transaction_id);
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        transaction_root_file(root)?.sync_all()?;
        Ok::<(), WorkspaceApplyError>(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn read_workspace_transaction_manifest(
    path: &Path,
) -> Result<WorkspaceTransactionManifest, WorkspaceApplyError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_WORKSPACE_TRANSACTION_MANIFEST_BYTES {
        return Err(WorkspaceApplyError::RecoveryRequired(
            "transaction manifest is not a bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_WORKSPACE_TRANSACTION_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_WORKSPACE_TRANSACTION_MANIFEST_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let manifest: WorkspaceTransactionManifest =
        serde_json::from_slice(&bytes).map_err(|error| {
            WorkspaceApplyError::RecoveryRequired(format!(
                "transaction manifest is invalid JSON: {error}"
            ))
        })?;
    validate_workspace_transaction_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_workspace_transaction_manifest(
    manifest: &WorkspaceTransactionManifest,
) -> Result<(), WorkspaceApplyError> {
    if manifest.schema_version != WORKSPACE_TRANSACTION_SCHEMA_VERSION
        || manifest.operation_revision == 0
        || !is_sha256(&manifest.result_digest)
        || manifest.members.is_empty()
        || manifest.members.len() > MAX_WORKSPACE_APPLY_FILES
        || manifest
            .failure_summary
            .as_ref()
            .is_some_and(|summary| summary.len() > 16 * 1024)
    {
        return Err(WorkspaceApplyError::RecoveryRequired(
            "transaction manifest header is invalid".into(),
        ));
    }
    let needs_staged_identity = matches!(
        manifest.phase,
        WorkspaceTransactionPhase::Prepared
            | WorkspaceTransactionPhase::RollingBack
            | WorkspaceTransactionPhase::Committed
    );
    let rolled_back_without_preparation = manifest.phase == WorkspaceTransactionPhase::RolledBack
        && manifest
            .members
            .iter()
            .all(|member| member.staged.is_none() && member.backup.is_none());
    let requires_staged_identity = needs_staged_identity
        || (manifest.phase == WorkspaceTransactionPhase::RolledBack
            && !rolled_back_without_preparation);
    let mut targets = BTreeSet::new();
    for (index, member) in manifest.members.iter().enumerate() {
        validate_target_path(&member.target_path)?;
        if !targets.insert(&member.target_path)
            || member.stage_name != workspace_stage_name(manifest.transaction_id, index)
            || member.backup_name
                != member
                    .base
                    .as_ref()
                    .map(|_| workspace_backup_name(manifest.transaction_id, index))
            || !is_sha256(&member.proposed_digest)
            || member.proposed_mode > 0o7777
            || (member.delete && member.base.is_none())
            || (requires_staged_identity && member.delete && member.staged.is_some())
            || (requires_staged_identity && !member.delete && member.staged.is_none())
            || (member.staged.is_some() && member.base.is_some() != member.backup.is_some())
            || (requires_staged_identity && member.base.is_some() != member.backup.is_some())
            || member
                .base
                .as_ref()
                .is_some_and(|identity| !is_sha256(&identity.digest))
            || member
                .staged
                .as_ref()
                .is_some_and(|identity| !is_sha256(&identity.digest))
            || member
                .backup
                .as_ref()
                .is_some_and(|identity| !is_sha256(&identity.digest))
        {
            return Err(WorkspaceApplyError::RecoveryRequired(
                "transaction manifest member is invalid".into(),
            ));
        }
        if let Some(staged) = member.staged.as_ref()
            && (staged.digest != member.proposed_digest
                || staged.mode & 0o7777 != member.proposed_mode)
        {
            return Err(WorkspaceApplyError::RecoveryRequired(
                "transaction staged identity does not match the proposal".into(),
            ));
        }
        if let (Some(base), Some(backup)) = (member.base.as_ref(), member.backup.as_ref())
            && base != backup
        {
            return Err(WorkspaceApplyError::RecoveryRequired(
                "transaction backup identity does not match the reviewed base".into(),
            ));
        }
    }
    Ok(())
}

fn workspace_stage_name(transaction_id: Uuid, index: usize) -> String {
    format!(".hyper-term-apply-{transaction_id}-{index}.tmp")
}

fn workspace_backup_name(transaction_id: Uuid, index: usize) -> String {
    format!(".hyper-term-apply-{transaction_id}-{index}.base")
}

fn stage_durable_workspace_plan(
    workspace: &Path,
    plan: &WorkspaceApplyPlan,
    member: &mut WorkspaceTransactionMember,
) -> Result<StagedWorkspacePlan, WorkspaceApplyError> {
    let relative = validate_target_path(&plan.target_path)?;
    let parent = open_parent(workspace, &relative)?;
    if parent.device != plan.parent_device
        || parent.inode != plan.parent_inode
        || parent.device != member.parent_device
        || parent.inode != member.parent_inode
    {
        return Err(WorkspaceApplyError::ParentChanged);
    }
    let parent_fd = parent.directory.as_raw_fd();
    let current = read_target_at(parent_fd, &parent.file_name)?;
    if !same_file_state(current.as_ref(), plan.base.as_ref()) {
        return Err(WorkspaceApplyError::StaleBase);
    }

    let stage_name =
        CString::new(member.stage_name.as_str()).map_err(|_| WorkspaceApplyError::InvalidPath)?;
    let backup_name = member
        .backup_name
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| WorkspaceApplyError::InvalidPath)?;
    let result = (|| {
        if !plan.delete {
            let mut stage = create_stage(parent_fd, &stage_name)?;
            stage.write_all(plan.proposed_bytes()?.as_ref())?;
            stage.set_permissions(fs::Permissions::from_mode(member.proposed_mode))?;
            stage.sync_all()?;
            drop(stage);
            let staged = read_target_at(parent_fd, &stage_name)?.ok_or(
                WorkspaceApplyError::RecoveryRequired(
                    "staged transaction member disappeared".into(),
                ),
            )?;
            if staged.digest != member.proposed_digest
                || staged.mode & 0o7777 != member.proposed_mode
            {
                return Err(WorkspaceApplyError::RecoveryRequired(
                    "staged transaction member could not be verified".into(),
                ));
            }
            member.staged = Some(WorkspaceRecoveryIdentity::from_snapshot(&staged));
        }
        if let Some(backup_name) = backup_name.as_ref() {
            link_target_at(parent_fd, &parent.file_name, backup_name)?;
            let backup = read_target_at(parent_fd, backup_name)?.ok_or(
                WorkspaceApplyError::RecoveryRequired(
                    "transaction backup disappeared while staging".into(),
                ),
            )?;
            let expected = member
                .base
                .as_ref()
                .ok_or(WorkspaceApplyError::RecoveryRequired(
                    "transaction backup has no reviewed base".into(),
                ))?;
            if !expected.matches(&backup) {
                return Err(WorkspaceApplyError::RecoveryRequired(
                    "transaction backup identity changed while staging".into(),
                ));
            }
            member.backup = Some(WorkspaceRecoveryIdentity::from_snapshot(&backup));
        }
        let latest = read_target_at(parent_fd, &parent.file_name)?;
        if !same_file_state(latest.as_ref(), plan.base.as_ref()) {
            return Err(WorkspaceApplyError::StaleBase);
        }
        parent.directory.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        if !plan.delete {
            unlink_at(parent_fd, &stage_name);
        }
        if let Some(backup_name) = backup_name.as_ref() {
            unlink_at(parent_fd, backup_name);
        }
        let _ = parent.directory.sync_all();
        member.staged = None;
        member.backup = None;
        return Err(error);
    }
    Ok(StagedWorkspacePlan {
        plan: plan.clone(),
        parent,
        stage_name,
        #[cfg(test)]
        backup_name,
        installed: false,
    })
}

fn recover_workspace_transaction(
    workspace: &Path,
    transaction_root: &Path,
    manifest: &mut WorkspaceTransactionManifest,
) -> Result<WorkspaceTransactionReceipt, WorkspaceApplyError> {
    match manifest.phase {
        WorkspaceTransactionPhase::Preparing => {
            cleanup_manifest_files(workspace, manifest)?;
            manifest.phase = WorkspaceTransactionPhase::RolledBack;
            manifest.failure_summary.get_or_insert_with(|| {
                "recovered an interrupted transaction before installation began".into()
            });
            write_workspace_transaction_manifest(transaction_root, manifest)?;
        }
        WorkspaceTransactionPhase::Prepared => {
            if all_manifest_targets_match(workspace, manifest, ManifestTargetState::Proposed)? {
                manifest.phase = WorkspaceTransactionPhase::Committed;
                write_workspace_transaction_manifest(transaction_root, manifest)?;
                cleanup_manifest_files(workspace, manifest)?;
            } else {
                manifest.phase = WorkspaceTransactionPhase::RollingBack;
                manifest.failure_summary.get_or_insert_with(|| {
                    "recovered an interrupted partial workspace transaction".into()
                });
                write_workspace_transaction_manifest(transaction_root, manifest)?;
                rollback_workspace_manifest(workspace, manifest)?;
                manifest.phase = WorkspaceTransactionPhase::RolledBack;
                write_workspace_transaction_manifest(transaction_root, manifest)?;
                cleanup_manifest_files(workspace, manifest)?;
            }
        }
        WorkspaceTransactionPhase::RollingBack => {
            rollback_workspace_manifest(workspace, manifest)?;
            manifest.phase = WorkspaceTransactionPhase::RolledBack;
            manifest.failure_summary.get_or_insert_with(|| {
                "continued rollback after an interrupted workspace transaction".into()
            });
            write_workspace_transaction_manifest(transaction_root, manifest)?;
            cleanup_manifest_files(workspace, manifest)?;
        }
        WorkspaceTransactionPhase::Committed => {
            if !all_manifest_targets_match(workspace, manifest, ManifestTargetState::Proposed)? {
                return Err(WorkspaceApplyError::RecoveryRequired(
                    "committed targets no longer match the durable transaction".into(),
                ));
            }
            cleanup_manifest_files(workspace, manifest)?;
        }
        WorkspaceTransactionPhase::RolledBack => {
            cleanup_manifest_files(workspace, manifest)?;
        }
    }
    Ok(manifest.receipt())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManifestTargetState {
    Base,
    Proposed,
    Unknown,
}

fn all_manifest_targets_match(
    workspace: &Path,
    manifest: &WorkspaceTransactionManifest,
    expected: ManifestTargetState,
) -> Result<bool, WorkspaceApplyError> {
    for member in &manifest.members {
        if classify_manifest_target(workspace, member)? != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn classify_manifest_target(
    workspace: &Path,
    member: &WorkspaceTransactionMember,
) -> Result<ManifestTargetState, WorkspaceApplyError> {
    let relative = validate_target_path(&member.target_path)?;
    let parent = open_parent(workspace, &relative)?;
    if parent.device != member.parent_device || parent.inode != member.parent_inode {
        return Err(WorkspaceApplyError::ParentChanged);
    }
    let current = read_target_at(parent.directory.as_raw_fd(), &parent.file_name)?;
    let is_base = match (current.as_ref(), member.base.as_ref()) {
        (None, None) => true,
        (Some(current), Some(base)) => base.matches(current),
        _ => false,
    };
    if is_base {
        return Ok(ManifestTargetState::Base);
    }
    if member.delete && current.is_none() {
        return Ok(ManifestTargetState::Proposed);
    }
    if let (Some(current), Some(staged)) = (current.as_ref(), member.staged.as_ref())
        && staged.matches(current)
    {
        return Ok(ManifestTargetState::Proposed);
    }
    Ok(ManifestTargetState::Unknown)
}

fn rollback_workspace_manifest(
    workspace: &Path,
    manifest: &WorkspaceTransactionManifest,
) -> Result<(), WorkspaceApplyError> {
    for member in manifest.members.iter().rev() {
        match classify_manifest_target(workspace, member)? {
            ManifestTargetState::Base => continue,
            ManifestTargetState::Unknown => {
                return Err(WorkspaceApplyError::RecoveryRequired(format!(
                    "target {} matches neither reviewed base nor staged proposal",
                    member.target_path
                )));
            }
            ManifestTargetState::Proposed => {}
        }
        let relative = validate_target_path(&member.target_path)?;
        let parent = open_parent(workspace, &relative)?;
        if parent.device != member.parent_device || parent.inode != member.parent_inode {
            return Err(WorkspaceApplyError::ParentChanged);
        }
        let parent_fd = parent.directory.as_raw_fd();
        match (member.base.as_ref(), member.backup_name.as_deref()) {
            (Some(base), Some(backup_name)) => {
                let backup_name =
                    CString::new(backup_name).map_err(|_| WorkspaceApplyError::InvalidPath)?;
                let backup = read_target_at(parent_fd, &backup_name)?.ok_or(
                    WorkspaceApplyError::RecoveryRequired(format!(
                        "backup for {} is missing",
                        member.target_path
                    )),
                )?;
                if !base.matches(&backup) {
                    return Err(WorkspaceApplyError::RecoveryRequired(format!(
                        "backup for {} changed identity",
                        member.target_path
                    )));
                }
                replace_target_at(parent_fd, &backup_name, &parent.file_name)?;
                let restored = read_target_at(parent_fd, &parent.file_name)?.ok_or(
                    WorkspaceApplyError::RecoveryRequired(format!(
                        "restored target {} disappeared",
                        member.target_path
                    )),
                )?;
                if !base.matches(&restored) {
                    return Err(WorkspaceApplyError::RecoveryRequired(format!(
                        "restored target {} has the wrong identity",
                        member.target_path
                    )));
                }
            }
            (None, None) => {
                unlink_at_if_exists(parent_fd, &parent.file_name)?;
                if read_target_at(parent_fd, &parent.file_name)?.is_some() {
                    return Err(WorkspaceApplyError::RecoveryRequired(format!(
                        "created target {} could not be removed",
                        member.target_path
                    )));
                }
            }
            _ => {
                return Err(WorkspaceApplyError::RecoveryRequired(
                    "transaction backup shape is invalid".into(),
                ));
            }
        }
        parent.directory.sync_all()?;
    }
    Ok(())
}

fn cleanup_manifest_files(
    workspace: &Path,
    manifest: &WorkspaceTransactionManifest,
) -> Result<(), WorkspaceApplyError> {
    for member in &manifest.members {
        let relative = validate_target_path(&member.target_path)?;
        let parent = open_parent(workspace, &relative)?;
        if parent.device != member.parent_device || parent.inode != member.parent_inode {
            return Err(WorkspaceApplyError::ParentChanged);
        }
        let parent_fd = parent.directory.as_raw_fd();
        let stage_name = CString::new(member.stage_name.as_str())
            .map_err(|_| WorkspaceApplyError::InvalidPath)?;
        if !member.delete {
            unlink_manifest_file_if_matches(
                parent_fd,
                &stage_name,
                member.staged.as_ref(),
                Some((&member.proposed_digest, member.proposed_mode)),
            )?;
        }
        if let Some(backup_name) = member.backup_name.as_deref() {
            let backup_name =
                CString::new(backup_name).map_err(|_| WorkspaceApplyError::InvalidPath)?;
            unlink_manifest_file_if_matches(
                parent_fd,
                &backup_name,
                member.backup.as_ref().or(member.base.as_ref()),
                None,
            )?;
        }
        parent.directory.sync_all()?;
    }
    Ok(())
}

fn unlink_manifest_file_if_matches(
    parent_fd: RawFd,
    name: &CStr,
    expected: Option<&WorkspaceRecoveryIdentity>,
    preparing_stage: Option<(&str, u32)>,
) -> Result<(), WorkspaceApplyError> {
    let Some(current) = read_target_at(parent_fd, name)? else {
        return Ok(());
    };
    let exact_identity = expected.is_some_and(|identity| identity.matches(&current));
    let exact_preparing_stage = preparing_stage
        .is_some_and(|(digest, mode)| current.digest == digest && current.mode & 0o7777 == mode);
    if !exact_identity && !exact_preparing_stage {
        return Err(WorkspaceApplyError::RecoveryRequired(
            "transaction cleanup entry changed identity".into(),
        ));
    }
    unlink_at_if_exists(parent_fd, name)
}

#[cfg(test)]
fn apply_single_workspace_plan(
    workspace: &Path,
    plan: &WorkspaceApplyPlan,
) -> Result<(), WorkspaceApplyError> {
    let relative = validate_target_path(&plan.target_path)?;
    let parent = open_parent(workspace, &relative)?;
    if parent.device != plan.parent_device || parent.inode != plan.parent_inode {
        return Err(WorkspaceApplyError::ParentChanged);
    }
    let parent_fd = parent.directory.as_raw_fd();
    let current = read_target_at(parent_fd, &parent.file_name)?;
    if !same_file_state(current.as_ref(), plan.base.as_ref()) {
        return Err(WorkspaceApplyError::StaleBase);
    }

    if plan.delete {
        unlink_at_checked(parent_fd, &parent.file_name)?;
        parent.directory.sync_all()?;
        if read_target_at(parent_fd, &parent.file_name)?.is_some() {
            return Err(WorkspaceApplyError::UnknownExecution(
                "deleted workspace target is still present".into(),
            ));
        }
        return Ok(());
    }

    let stage_name = CString::new(format!(".hyper-term-apply-{}.tmp", Uuid::new_v4()))
        .expect("generated workspace stage names do not contain NUL");
    let mut stage = create_stage(parent_fd, &stage_name)?;
    let mut installed = false;
    let install_result = (|| {
        stage.write_all(plan.proposed_bytes()?.as_ref())?;
        let mode = plan
            .base
            .as_ref()
            .map(|snapshot| snapshot.mode & 0o7777)
            .unwrap_or(0o644);
        stage.set_permissions(fs::Permissions::from_mode(mode))?;
        stage.sync_all()?;

        let latest = read_target_at(parent_fd, &parent.file_name)?;
        if !same_file_state(latest.as_ref(), plan.base.as_ref()) {
            return Err(WorkspaceApplyError::StaleBase);
        }
        install_stage(
            parent_fd,
            &stage_name,
            &parent.file_name,
            plan.base.as_ref(),
        )?;
        installed = true;
        parent.directory.sync_all()?;
        let installed =
            read_target_at(parent_fd, &parent.file_name)?.ok_or(WorkspaceApplyError::StaleBase)?;
        if installed.digest != plan.proposed_digest {
            return Err(WorkspaceApplyError::StaleBase);
        }
        Ok(())
    })();
    drop(stage);
    if install_result.is_err() {
        unlink_at(parent_fd, &stage_name);
    }
    if let Err(error) = install_result {
        return if installed {
            Err(WorkspaceApplyError::UnknownExecution(error.to_string()))
        } else {
            Err(error)
        };
    }
    Ok(())
}

struct StagedWorkspacePlan {
    plan: WorkspaceApplyPlan,
    parent: OpenParent,
    stage_name: CString,
    #[cfg(test)]
    backup_name: Option<CString>,
    installed: bool,
}

#[cfg(test)]
fn apply_workspace_transaction(
    workspace: &Path,
    set: &WorkspaceApplySetPlan,
) -> Result<(), WorkspaceApplyError> {
    let mut staged = Vec::with_capacity(set.plans.len());
    for plan in &set.plans {
        match stage_workspace_plan(workspace, plan) {
            Ok(candidate) => staged.push(candidate),
            Err(error) => {
                cleanup_staged_workspace_plans(&staged);
                return Err(error);
            }
        }
    }

    for index in 0..staged.len() {
        let install_result = install_transaction_plan(&mut staged[index]);
        if let Err(error) = install_result {
            let rollback = rollback_workspace_transaction(&mut staged);
            cleanup_staged_workspace_plans(&staged);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(WorkspaceApplyError::UnknownExecution(format!(
                    "{error}; rollback failed: {rollback_error}"
                ))),
            };
        }
    }

    for staged_plan in &staged {
        if let Some(backup_name) = staged_plan.backup_name.as_ref()
            && let Err(error) =
                unlink_at_checked(staged_plan.parent.directory.as_raw_fd(), backup_name).and_then(
                    |()| {
                        staged_plan
                            .parent
                            .directory
                            .sync_all()
                            .map_err(WorkspaceApplyError::from)
                    },
                )
        {
            return Err(WorkspaceApplyError::UnknownExecution(format!(
                "targets were installed but transaction cleanup failed: {error}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
fn stage_workspace_plan(
    workspace: &Path,
    plan: &WorkspaceApplyPlan,
) -> Result<StagedWorkspacePlan, WorkspaceApplyError> {
    let relative = validate_target_path(&plan.target_path)?;
    let parent = open_parent(workspace, &relative)?;
    if parent.device != plan.parent_device || parent.inode != plan.parent_inode {
        return Err(WorkspaceApplyError::ParentChanged);
    }
    let parent_fd = parent.directory.as_raw_fd();
    let current = read_target_at(parent_fd, &parent.file_name)?;
    if !same_file_state(current.as_ref(), plan.base.as_ref()) {
        return Err(WorkspaceApplyError::StaleBase);
    }

    let transaction_id = Uuid::new_v4();
    let stage_name = CString::new(format!(".hyper-term-apply-{transaction_id}.tmp"))
        .expect("generated workspace stage names do not contain NUL");
    let backup_name = plan.base.as_ref().map(|_| {
        CString::new(format!(".hyper-term-apply-{transaction_id}.base"))
            .expect("generated workspace backup names do not contain NUL")
    });
    let result = (|| {
        if !plan.delete {
            let mut stage = create_stage(parent_fd, &stage_name)?;
            stage.write_all(plan.proposed_bytes()?.as_ref())?;
            let mode = plan
                .base
                .as_ref()
                .map(|snapshot| snapshot.mode & 0o7777)
                .unwrap_or(0o644);
            stage.set_permissions(fs::Permissions::from_mode(mode))?;
            stage.sync_all()?;
            drop(stage);
        }
        if let Some(backup_name) = backup_name.as_ref() {
            link_target_at(parent_fd, &parent.file_name, backup_name)?;
        }
        let latest = read_target_at(parent_fd, &parent.file_name)?;
        if !same_file_state(latest.as_ref(), plan.base.as_ref()) {
            return Err(WorkspaceApplyError::StaleBase);
        }
        Ok(())
    })();
    if let Err(error) = result {
        if !plan.delete {
            unlink_at(parent_fd, &stage_name);
        }
        if let Some(backup_name) = backup_name.as_ref() {
            unlink_at(parent_fd, backup_name);
        }
        return Err(error);
    }
    Ok(StagedWorkspacePlan {
        plan: plan.clone(),
        parent,
        stage_name,
        backup_name,
        installed: false,
    })
}

fn install_transaction_plan(staged: &mut StagedWorkspacePlan) -> Result<(), WorkspaceApplyError> {
    let parent_fd = staged.parent.directory.as_raw_fd();
    let latest = read_target_at(parent_fd, &staged.parent.file_name)?;
    if !same_file_state(latest.as_ref(), staged.plan.base.as_ref()) {
        return Err(WorkspaceApplyError::StaleBase);
    }
    if staged.plan.delete {
        unlink_at_checked(parent_fd, &staged.parent.file_name)?;
    } else {
        install_transaction_stage(
            parent_fd,
            &staged.stage_name,
            &staged.parent.file_name,
            staged.plan.base.is_some(),
        )?;
    }
    staged.installed = true;
    staged.parent.directory.sync_all()?;
    let installed = read_target_at(parent_fd, &staged.parent.file_name)?;
    if staged.plan.delete {
        if installed.is_some() {
            return Err(WorkspaceApplyError::StaleBase);
        }
    } else {
        let installed = installed.ok_or(WorkspaceApplyError::StaleBase)?;
        if installed.digest != staged.plan.proposed_digest
            || installed.mode & 0o7777
                != staged
                    .plan
                    .base
                    .as_ref()
                    .map(|base| base.mode & 0o7777)
                    .unwrap_or(0o644)
        {
            return Err(WorkspaceApplyError::StaleBase);
        }
    }
    Ok(())
}

#[cfg(test)]
fn rollback_workspace_transaction(
    staged: &mut [StagedWorkspacePlan],
) -> Result<(), WorkspaceApplyError> {
    for staged_plan in staged.iter_mut().rev().filter(|plan| plan.installed) {
        let parent_fd = staged_plan.parent.directory.as_raw_fd();
        let current = read_target_at(parent_fd, &staged_plan.parent.file_name)?;
        let expected_digest = if staged_plan.plan.delete {
            None
        } else {
            Some(staged_plan.plan.proposed_digest.as_str())
        };
        if current.as_ref().map(|snapshot| snapshot.digest.as_str()) != expected_digest {
            return Err(WorkspaceApplyError::StaleBase);
        }
        match (
            staged_plan.plan.base.as_ref(),
            staged_plan.backup_name.as_ref(),
        ) {
            (Some(base), Some(backup_name)) => {
                let backup = read_target_at(parent_fd, backup_name)?;
                if !same_file_state(backup.as_ref(), Some(base)) {
                    return Err(WorkspaceApplyError::StaleBase);
                }
                replace_target_at(parent_fd, backup_name, &staged_plan.parent.file_name)?;
                let restored = read_target_at(parent_fd, &staged_plan.parent.file_name)?;
                if !same_file_state(restored.as_ref(), Some(base)) {
                    return Err(WorkspaceApplyError::StaleBase);
                }
            }
            (None, None) => {
                unlink_at_checked(parent_fd, &staged_plan.parent.file_name)?;
                if read_target_at(parent_fd, &staged_plan.parent.file_name)?.is_some() {
                    return Err(WorkspaceApplyError::StaleBase);
                }
            }
            _ => return Err(WorkspaceApplyError::StaleBase),
        }
        staged_plan.parent.directory.sync_all()?;
        staged_plan.installed = false;
    }
    Ok(())
}

#[cfg(test)]
fn cleanup_staged_workspace_plans(staged: &[StagedWorkspacePlan]) {
    for staged_plan in staged {
        let parent_fd = staged_plan.parent.directory.as_raw_fd();
        if !staged_plan.installed && !staged_plan.plan.delete {
            unlink_at(parent_fd, &staged_plan.stage_name);
        }
        if let Some(backup_name) = staged_plan.backup_name.as_ref() {
            unlink_at(parent_fd, backup_name);
        }
    }
}

fn workspace_set_digest(plans: &[WorkspaceApplyPlan]) -> String {
    if plans.iter().all(|plan| !plan.delete) {
        if let [plan] = plans {
            return plan.proposed_digest.clone();
        }
        let mut digest = Sha256::new();
        digest.update(b"hyper-term.workspace.apply-set.v1\0");
        for plan in plans {
            digest.update((plan.target_path.len() as u64).to_be_bytes());
            digest.update(plan.target_path.as_bytes());
            digest.update(plan.proposed_digest.as_bytes());
        }
        return sha256_digest(digest.finalize());
    }
    let mut digest = Sha256::new();
    digest.update(b"hyper-term.workspace.apply-set.v2\0");
    for plan in plans {
        digest.update((plan.target_path.len() as u64).to_be_bytes());
        digest.update(plan.target_path.as_bytes());
        digest.update([u8::from(plan.delete)]);
        digest.update(plan.proposed_digest.as_bytes());
    }
    sha256_digest(digest.finalize())
}

fn workspace_deletion_digest() -> String {
    sha256_bytes(b"hyper-term.workspace.delete.v1\0")
}

#[cfg(test)]
#[path = "workspace_apply/tests.rs"]
mod tests;
