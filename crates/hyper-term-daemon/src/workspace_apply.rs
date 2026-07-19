use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub(crate) const MAX_WORKSPACE_FILE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_WORKSPACE_APPLY_FILES: usize = 32;
pub(crate) const MAX_WORKSPACE_APPLY_BYTES: usize = 4 * 1024 * 1024;
const MAX_TARGET_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceFileSnapshot {
    pub content: String,
    pub digest: String,
    device: u64,
    inode: u64,
    mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceApplyPlan {
    pub target_path: String,
    pub proposed_content: String,
    pub proposed_digest: String,
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceApplySetPlan {
    pub plans: Vec<WorkspaceApplyPlan>,
    pub result_digest: String,
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
    if requests.is_empty() || requests.len() > MAX_WORKSPACE_APPLY_FILES {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    let total_bytes = requests
        .iter()
        .try_fold(0_usize, |total, (_, content)| {
            total.checked_add(content.len())
        })
        .ok_or(WorkspaceApplyError::TooLarge)?;
    if total_bytes > MAX_WORKSPACE_APPLY_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let mut targets = BTreeSet::new();
    let mut plans = Vec::with_capacity(requests.len());
    for (target_path, proposed_content) in requests {
        if !targets.insert(target_path.clone()) {
            return Err(WorkspaceApplyError::InvalidPath);
        }
        match prepare_workspace_file(workspace, &target_path, proposed_content) {
            Ok(plan) => plans.push(plan),
            Err(WorkspaceApplyError::NoChanges) => {}
            Err(error) => return Err(error),
        }
    }
    if plans.is_empty() {
        return Err(WorkspaceApplyError::NoChanges);
    }
    let base_bytes = plans.iter().try_fold(0_usize, |total, plan| {
        total.checked_add(plan.base_content().len())
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
    if proposed_content.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let relative = validate_target_path(target_path)?;
    let parent = open_parent(workspace, &relative)?;
    let base = read_target_at(parent.directory.as_raw_fd(), &parent.file_name)?;
    if base
        .as_ref()
        .is_some_and(|snapshot| snapshot.content == proposed_content)
    {
        return Err(WorkspaceApplyError::NoChanges);
    }
    Ok(WorkspaceApplyPlan {
        target_path: target_path.to_owned(),
        proposed_digest: sha256_bytes(proposed_content.as_bytes()),
        proposed_content,
        base,
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
        result_digest: plan.proposed_digest.clone(),
    };
    apply_workspace_set_plan(workspace, &set)
}

pub(crate) fn apply_workspace_set_plan(
    workspace: &Path,
    set: &WorkspaceApplySetPlan,
) -> Result<String, WorkspaceApplyError> {
    if set.plans.is_empty()
        || set.plans.len() > MAX_WORKSPACE_APPLY_FILES
        || workspace_set_digest(&set.plans) != set.result_digest
    {
        return Err(WorkspaceApplyError::InvalidPath);
    }
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

    let stage_name = CString::new(format!(".hyper-term-apply-{}.tmp", Uuid::new_v4()))
        .expect("generated workspace stage names do not contain NUL");
    let mut stage = create_stage(parent_fd, &stage_name)?;
    let mut installed = false;
    let install_result = (|| {
        stage.write_all(plan.proposed_content.as_bytes())?;
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
    backup_name: Option<CString>,
    installed: bool,
}

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
        if let Some(backup_name) = staged_plan.backup_name.as_ref() {
            if let Err(error) =
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
    }
    Ok(())
}

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
        let mut stage = create_stage(parent_fd, &stage_name)?;
        stage.write_all(plan.proposed_content.as_bytes())?;
        let mode = plan
            .base
            .as_ref()
            .map(|snapshot| snapshot.mode & 0o7777)
            .unwrap_or(0o644);
        stage.set_permissions(fs::Permissions::from_mode(mode))?;
        stage.sync_all()?;
        drop(stage);
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
        unlink_at(parent_fd, &stage_name);
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
    install_transaction_stage(
        parent_fd,
        &staged.stage_name,
        &staged.parent.file_name,
        staged.plan.base.is_some(),
    )?;
    staged.installed = true;
    staged.parent.directory.sync_all()?;
    let installed = read_target_at(parent_fd, &staged.parent.file_name)?
        .ok_or(WorkspaceApplyError::StaleBase)?;
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
    Ok(())
}

fn rollback_workspace_transaction(
    staged: &mut [StagedWorkspacePlan],
) -> Result<(), WorkspaceApplyError> {
    for staged_plan in staged.iter_mut().rev().filter(|plan| plan.installed) {
        let parent_fd = staged_plan.parent.directory.as_raw_fd();
        let current = read_target_at(parent_fd, &staged_plan.parent.file_name)?;
        if current.as_ref().map(|snapshot| snapshot.digest.as_str())
            != Some(staged_plan.plan.proposed_digest.as_str())
        {
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

fn cleanup_staged_workspace_plans(staged: &[StagedWorkspacePlan]) {
    for staged_plan in staged {
        let parent_fd = staged_plan.parent.directory.as_raw_fd();
        if !staged_plan.installed {
            unlink_at(parent_fd, &staged_plan.stage_name);
        }
        if let Some(backup_name) = staged_plan.backup_name.as_ref() {
            unlink_at(parent_fd, backup_name);
        }
    }
}

fn workspace_set_digest(plans: &[WorkspaceApplyPlan]) -> String {
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
    sha256_digest(digest.finalize())
}

fn validate_target_path(value: &str) -> Result<PathBuf, WorkspaceApplyError> {
    if value.is_empty()
        || value.len() > MAX_TARGET_PATH_BYTES
        || value.as_bytes().contains(&0)
        || Path::new(value).is_absolute()
    {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    let path = PathBuf::from(value);
    let mut count = 0_usize;
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(WorkspaceApplyError::InvalidPath);
        };
        if matches!(name.to_str(), Some(".git" | ".hg" | ".svn" | ".jj")) {
            return Err(WorkspaceApplyError::InvalidPath);
        }
        count += 1;
    }
    if count == 0 || path.file_name().is_none() {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    Ok(path)
}

fn open_parent(workspace: &Path, relative: &Path) -> Result<OpenParent, WorkspaceApplyError> {
    let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
    let mut parent_path = workspace.to_path_buf();
    for component in parent_relative.components() {
        let Component::Normal(name) = component else {
            return Err(WorkspaceApplyError::InvalidPath);
        };
        parent_path.push(name);
        let metadata = fs::symlink_metadata(&parent_path)
            .map_err(|_| WorkspaceApplyError::ParentUnavailable)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkspaceApplyError::ParentUnavailable);
        }
    }
    let canonical_parent = parent_path
        .canonicalize()
        .map_err(|_| WorkspaceApplyError::ParentUnavailable)?;
    if !canonical_parent.starts_with(workspace) {
        return Err(WorkspaceApplyError::InvalidPath);
    }
    let expected =
        fs::metadata(&canonical_parent).map_err(|_| WorkspaceApplyError::ParentUnavailable)?;
    if !expected.is_dir() {
        return Err(WorkspaceApplyError::ParentUnavailable);
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical_parent)
        .map_err(|_| WorkspaceApplyError::ParentUnavailable)?;
    let opened = directory
        .metadata()
        .map_err(|_| WorkspaceApplyError::ParentUnavailable)?;
    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err(WorkspaceApplyError::ParentChanged);
    }
    let file_name = c_string(
        relative
            .file_name()
            .ok_or(WorkspaceApplyError::InvalidPath)?,
    )?;
    Ok(OpenParent {
        directory,
        file_name,
        device: opened.dev(),
        inode: opened.ino(),
    })
}

fn read_target_at(
    parent_fd: RawFd,
    file_name: &CStr,
) -> Result<Option<WorkspaceFileSnapshot>, WorkspaceApplyError> {
    // SAFETY: parent_fd is an open directory descriptor and file_name is a bounded C string.
    let descriptor = unsafe {
        libc::openat(
            parent_fd,
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(WorkspaceApplyError::UnsupportedTarget);
    }
    // SAFETY: openat returned a new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(WorkspaceApplyError::UnsupportedTarget);
    }
    if metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(WorkspaceApplyError::TooLarge);
    }
    let content = String::from_utf8(bytes).map_err(|_| WorkspaceApplyError::UnsupportedTarget)?;
    Ok(Some(WorkspaceFileSnapshot {
        digest: sha256_bytes(content.as_bytes()),
        content,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
    }))
}

fn create_stage(parent_fd: RawFd, stage_name: &CStr) -> Result<File, WorkspaceApplyError> {
    // SAFETY: parent_fd is an open directory descriptor and stage_name is generated locally.
    let descriptor = unsafe {
        libc::openat(
            parent_fd,
            stage_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(WorkspaceApplyError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(target_os = "macos")]
fn install_stage(
    parent_fd: RawFd,
    stage_name: &CStr,
    target_name: &CStr,
    base: Option<&WorkspaceFileSnapshot>,
) -> Result<(), WorkspaceApplyError> {
    let flags = if base.is_some() {
        libc::RENAME_SWAP
    } else {
        libc::RENAME_EXCL
    };
    // SAFETY: both names are relative to the same open parent directory descriptor.
    let result = unsafe {
        libc::renameatx_np(
            parent_fd,
            stage_name.as_ptr(),
            parent_fd,
            target_name.as_ptr(),
            flags,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::EEXIST) {
            Err(WorkspaceApplyError::StaleBase)
        } else {
            Err(WorkspaceApplyError::Io(error))
        };
    }
    if let Some(base) = base {
        let displaced = read_target_at(parent_fd, stage_name)?;
        if !same_file_state(displaced.as_ref(), Some(base)) {
            // SAFETY: a second swap restores the two directory entries after a detected race.
            let rollback = unsafe {
                libc::renameatx_np(
                    parent_fd,
                    stage_name.as_ptr(),
                    parent_fd,
                    target_name.as_ptr(),
                    libc::RENAME_SWAP,
                )
            };
            if rollback != 0 {
                return Err(WorkspaceApplyError::UnknownExecution(
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            return Err(WorkspaceApplyError::StaleBase);
        }
        unlink_at(parent_fd, stage_name);
    }
    Ok(())
}

fn link_target_at(
    parent_fd: RawFd,
    target_name: &CStr,
    backup_name: &CStr,
) -> Result<(), WorkspaceApplyError> {
    // SAFETY: both names are bounded and relative to the same open directory.
    let result = unsafe {
        libc::linkat(
            parent_fd,
            target_name.as_ptr(),
            parent_fd,
            backup_name.as_ptr(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(WorkspaceApplyError::Io(std::io::Error::last_os_error()))
    }
}

#[cfg(target_os = "macos")]
fn install_transaction_stage(
    parent_fd: RawFd,
    stage_name: &CStr,
    target_name: &CStr,
    replacing: bool,
) -> Result<(), WorkspaceApplyError> {
    let result = if replacing {
        // SAFETY: both names are bounded and relative to the same open directory.
        unsafe {
            libc::renameat(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
            )
        }
    } else {
        // SAFETY: both names are bounded and relative to the same open directory.
        unsafe {
            libc::renameatx_np(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        }
    };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            Err(WorkspaceApplyError::StaleBase)
        } else {
            Err(WorkspaceApplyError::Io(error))
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn install_transaction_stage(
    parent_fd: RawFd,
    stage_name: &CStr,
    target_name: &CStr,
    replacing: bool,
) -> Result<(), WorkspaceApplyError> {
    let result = if replacing {
        // SAFETY: both names are bounded and relative to the same open directory.
        unsafe {
            libc::renameat(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
            )
        }
    } else {
        // linkat supplies an atomic no-replace boundary where renameat2 is not portable.
        // SAFETY: both names are bounded and relative to the same open directory.
        let linked = unsafe {
            libc::linkat(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                0,
            )
        };
        if linked == 0 {
            unlink_at(parent_fd, stage_name);
        }
        linked
    };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            Err(WorkspaceApplyError::StaleBase)
        } else {
            Err(WorkspaceApplyError::Io(error))
        }
    }
}

fn replace_target_at(
    parent_fd: RawFd,
    source_name: &CStr,
    target_name: &CStr,
) -> Result<(), WorkspaceApplyError> {
    // SAFETY: both names are bounded and relative to the same open directory.
    let result = unsafe {
        libc::renameat(
            parent_fd,
            source_name.as_ptr(),
            parent_fd,
            target_name.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(WorkspaceApplyError::Io(std::io::Error::last_os_error()))
    }
}

#[cfg(not(target_os = "macos"))]
fn install_stage(
    parent_fd: RawFd,
    stage_name: &CStr,
    target_name: &CStr,
    base: Option<&WorkspaceFileSnapshot>,
) -> Result<(), WorkspaceApplyError> {
    let result = if base.is_none() {
        // linkat gives creation an atomic no-replace boundary on non-macOS test hosts.
        // SAFETY: both names are relative to the same open parent directory descriptor.
        let linked = unsafe {
            libc::linkat(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                0,
            )
        };
        if linked == 0 {
            unlink_at(parent_fd, stage_name);
        }
        linked
    } else {
        // SAFETY: both names are relative to the same open parent directory descriptor.
        unsafe {
            libc::renameat(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
            )
        }
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::EEXIST) {
            Err(WorkspaceApplyError::StaleBase)
        } else {
            Err(WorkspaceApplyError::Io(error))
        };
    }
    Ok(())
}

fn same_file_state(
    current: Option<&WorkspaceFileSnapshot>,
    expected: Option<&WorkspaceFileSnapshot>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(current), Some(expected)) => {
            current.digest == expected.digest
                && current.device == expected.device
                && current.inode == expected.inode
                && current.mode == expected.mode
        }
        _ => false,
    }
}

fn c_string(value: &OsStr) -> Result<CString, WorkspaceApplyError> {
    CString::new(value.as_bytes()).map_err(|_| WorkspaceApplyError::InvalidPath)
}

fn unlink_at(parent_fd: RawFd, name: &CStr) {
    // SAFETY: name is relative to an open directory descriptor; cleanup is best effort.
    let _ = unsafe { libc::unlinkat(parent_fd, name.as_ptr(), 0) };
}

fn unlink_at_checked(parent_fd: RawFd, name: &CStr) -> Result<(), WorkspaceApplyError> {
    // SAFETY: name is bounded and relative to an open directory descriptor.
    let result = unsafe { libc::unlinkat(parent_fd, name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(WorkspaceApplyError::Io(std::io::Error::last_os_error()))
    }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    sha256_digest(Sha256::digest(bytes))
}

fn sha256_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
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
}
