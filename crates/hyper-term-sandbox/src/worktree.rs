use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const WORKTREE_SCHEMA_VERSION: u32 = 1;
const MAX_TASK_ID_BYTES: usize = 64;
const MAX_SOURCE_FILES: usize = 50_000;
const MAX_SOURCE_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// A request to materialize one immutable Git commit as a private Tier 2 input.
///
/// The user's current working tree is never copied. `revision` resolves to an
/// exact commit, so tracked dirty changes and untracked files remain outside
/// the isolated environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedWorktreeRequest {
    pub source_workspace: PathBuf,
    pub state_root: PathBuf,
    pub task_id: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedWorktreeManifest {
    pub schema_version: u32,
    pub environment_id: String,
    pub task_id: String,
    pub source_workspace: PathBuf,
    pub source_revision: String,
    pub worktree: PathBuf,
    pub file_count: usize,
    pub source_bytes: u64,
    pub inventory_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedWorktree {
    pub environment_root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: IsolatedWorktreeManifest,
}

#[derive(Clone, Debug)]
pub struct IsolatedWorktreeManager {
    git_executable: PathBuf,
}

impl IsolatedWorktreeManager {
    pub fn discover() -> Result<Self, IsolatedWorktreeError> {
        Ok(Self {
            git_executable: discover_git_executable()?,
        })
    }

    pub fn with_git_executable(
        git_executable: impl AsRef<Path>,
    ) -> Result<Self, IsolatedWorktreeError> {
        let git_executable = fs::canonicalize(git_executable.as_ref()).map_err(|error| {
            IsolatedWorktreeError::InvalidGitExecutable {
                path: git_executable.as_ref().to_path_buf(),
                error,
            }
        })?;
        if !fs::metadata(&git_executable)?.is_file() {
            return Err(IsolatedWorktreeError::GitExecutableNotFile(git_executable));
        }
        Ok(Self { git_executable })
    }

    pub fn create(
        &self,
        request: &IsolatedWorktreeRequest,
    ) -> Result<IsolatedWorktree, IsolatedWorktreeError> {
        validate_task_id(&request.task_id)?;
        let source_workspace = canonical_git_root(self, &request.source_workspace)?;
        let state_root = prepare_private_state_root(&request.state_root)?;
        reject_nested_roots(&source_workspace, &state_root)?;
        let source_revision = resolve_commit(
            self,
            &source_workspace,
            request.revision.as_deref().unwrap_or("HEAD"),
        )?;
        let environment_id = environment_id(&request.task_id, &source_workspace, &source_revision);
        let environment_root = state_root.join(&environment_id);
        let worktree = environment_root.join("worktree");
        let manifest_path = environment_root.join("manifest.json");

        create_private_directory(&environment_root).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                IsolatedWorktreeError::EnvironmentAlreadyExists(environment_root.clone())
            } else {
                IsolatedWorktreeError::Io(error)
            }
        })?;

        let create_result = (|| {
            self.git(
                &source_workspace,
                [
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    OsStr::new("--detach"),
                    OsStr::new("--no-checkout"),
                    worktree.as_os_str(),
                    OsStr::new(&source_revision),
                ],
            )?;
            self.git(
                &worktree,
                [OsStr::new("read-tree"), OsStr::new(&source_revision)],
            )?;
            let inventory =
                materialize_commit(self, &source_workspace, &source_revision, &worktree)?;
            verify_clean_worktree(self, &worktree)?;

            let manifest = IsolatedWorktreeManifest {
                schema_version: WORKTREE_SCHEMA_VERSION,
                environment_id: environment_id.clone(),
                task_id: request.task_id.clone(),
                source_workspace: source_workspace.clone(),
                source_revision,
                worktree: worktree.clone(),
                file_count: inventory.file_count,
                source_bytes: inventory.source_bytes,
                inventory_sha256: inventory.digest,
            };
            write_manifest(&manifest_path, &manifest)?;
            Ok(IsolatedWorktree {
                environment_root: environment_root.clone(),
                manifest_path,
                manifest,
            })
        })();

        if create_result.is_err() {
            let _ = self.remove_registered_worktree(&source_workspace, &worktree);
            let _ = fs::remove_dir_all(&environment_root);
        }
        create_result
    }

    pub fn destroy(&self, environment: &IsolatedWorktree) -> Result<(), IsolatedWorktreeError> {
        let state_root = environment.environment_root.parent().ok_or_else(|| {
            IsolatedWorktreeError::UnsafeCleanup(environment.environment_root.clone())
        })?;
        let canonical_state_root = fs::canonicalize(state_root)?;
        let canonical_environment_root = fs::canonicalize(&environment.environment_root)?;
        if canonical_environment_root.parent() != Some(canonical_state_root.as_path())
            || canonical_environment_root.file_name()
                != Some(OsStr::new(&environment.manifest.environment_id))
            || environment.manifest.worktree != environment.environment_root.join("worktree")
        {
            return Err(IsolatedWorktreeError::UnsafeCleanup(
                environment.environment_root.clone(),
            ));
        }
        let stored = read_manifest(&environment.manifest_path)?;
        if stored != environment.manifest {
            return Err(IsolatedWorktreeError::ManifestMismatch);
        }
        self.remove_registered_worktree(
            &environment.manifest.source_workspace,
            &environment.manifest.worktree,
        )?;
        fs::remove_dir_all(&canonical_environment_root)?;
        Ok(())
    }

    fn remove_registered_worktree(
        &self,
        source_workspace: &Path,
        worktree: &Path,
    ) -> Result<(), IsolatedWorktreeError> {
        if !worktree.exists() {
            return Ok(());
        }
        self.git(
            source_workspace,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                worktree.as_os_str(),
            ],
        )?;
        Ok(())
    }

    fn git<I, S>(&self, cwd: &Path, arguments: I) -> Result<Output, IsolatedWorktreeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.git_command(cwd);
        let output = command.args(arguments).output()?;
        if output.stdout.len() > MAX_GIT_OUTPUT_BYTES || output.stderr.len() > MAX_GIT_OUTPUT_BYTES
        {
            return Err(IsolatedWorktreeError::GitOutputTooLarge);
        }
        if !output.status.success() {
            return Err(IsolatedWorktreeError::GitFailed {
                status: output.status.code(),
                stderr: bounded_diagnostic(&output.stderr),
            });
        }
        Ok(output)
    }

    fn read_blobs(
        &self,
        cwd: &Path,
        entries: &[TreeEntry],
    ) -> Result<Vec<Vec<u8>>, IsolatedWorktreeError> {
        let mut command = self.git_command(cwd);
        let mut child = command
            .arg("cat-file")
            .arg("--batch")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            IsolatedWorktreeError::Io(std::io::Error::other("Git cat-file stdin is unavailable"))
        })?;
        let object_ids = entries
            .iter()
            .map(|entry| entry.object_id.clone())
            .collect::<Vec<_>>();
        let writer = std::thread::spawn(move || -> Result<(), std::io::Error> {
            for object_id in object_ids {
                stdin.write_all(object_id.as_bytes())?;
                stdin.write_all(b"\n")?;
            }
            Ok(())
        });
        let output = child.wait_with_output()?;
        writer.join().map_err(|_| {
            IsolatedWorktreeError::Io(std::io::Error::other("Git cat-file input writer panicked"))
        })??;
        if output.stderr.len() > MAX_GIT_OUTPUT_BYTES {
            return Err(IsolatedWorktreeError::GitOutputTooLarge);
        }
        if !output.status.success() {
            return Err(IsolatedWorktreeError::GitFailed {
                status: output.status.code(),
                stderr: bounded_diagnostic(&output.stderr),
            });
        }
        let overhead_bound = entries.len().saturating_mul(128);
        if output.stdout.len() > MAX_SOURCE_BYTES.saturating_add(overhead_bound) {
            return Err(IsolatedWorktreeError::SourceTooLarge);
        }
        parse_batch_blobs(&output.stdout, entries)
    }

    fn git_command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.git_executable);
        command
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("core.untrackedCache=false")
            .arg("-C")
            .arg(cwd);
        command
    }
}

#[derive(Debug, Error)]
pub enum IsolatedWorktreeError {
    #[error("Git executable {0} is not a regular file")]
    GitExecutableNotFile(PathBuf),
    #[error("cannot resolve Git executable {path}: {error}")]
    InvalidGitExecutable {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("Git is unavailable")]
    GitUnavailable,
    #[error("Git command failed with status {status:?}: {stderr}")]
    GitFailed { status: Option<i32>, stderr: String },
    #[error("Git command output exceeded the isolation bound")]
    GitOutputTooLarge,
    #[error("task id must contain 1 to 64 ASCII letters, digits, '-' or '_'")]
    InvalidTaskId,
    #[error("source workspace and private state root must not contain one another")]
    NestedRoots,
    #[error("private state root is not a private, non-symlinked directory: {0}")]
    InsecureStateRoot(PathBuf),
    #[error("isolated environment already exists at {0}")]
    EnvironmentAlreadyExists(PathBuf),
    #[error("revision did not resolve to one full commit id")]
    InvalidRevision,
    #[error("unsupported Git tree entry: {0}")]
    UnsupportedTreeEntry(String),
    #[error("unsafe Git path in source revision: {0}")]
    UnsafeSourcePath(String),
    #[error("unsafe symlink target in source revision: {0}")]
    UnsafeSymlink(String),
    #[error("source revision exceeds {MAX_SOURCE_FILES} files")]
    TooManyFiles,
    #[error("source file exceeds {MAX_SOURCE_FILE_BYTES} bytes: {0}")]
    SourceFileTooLarge(String),
    #[error("source revision exceeds {MAX_SOURCE_BYTES} bytes")]
    SourceTooLarge,
    #[error("materialized worktree does not exactly match the source commit")]
    WorktreeNotClean,
    #[error("refusing unsafe isolated-environment cleanup at {0}")]
    UnsafeCleanup(PathBuf),
    #[error("isolated-environment manifest does not match the live handle")]
    ManifestMismatch,
    #[error("invalid isolated-environment manifest: {0}")]
    InvalidManifest(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
struct Inventory {
    file_count: usize,
    source_bytes: u64,
    digest: String,
}

#[derive(Debug)]
struct TreeEntry {
    mode: u32,
    object_id: String,
    path: PathBuf,
}

fn canonical_git_root(
    manager: &IsolatedWorktreeManager,
    workspace: &Path,
) -> Result<PathBuf, IsolatedWorktreeError> {
    let workspace = fs::canonicalize(workspace)?;
    let output = manager.git(
        &workspace,
        [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
    )?;
    let root = output_line(&output.stdout)?;
    fs::canonicalize(root).map_err(IsolatedWorktreeError::Io)
}

fn resolve_commit(
    manager: &IsolatedWorktreeManager,
    workspace: &Path,
    revision: &str,
) -> Result<String, IsolatedWorktreeError> {
    if revision.is_empty() || revision.len() > 256 || revision.as_bytes().contains(&0) {
        return Err(IsolatedWorktreeError::InvalidRevision);
    }
    let expression = format!("{revision}^{{commit}}");
    let output = manager.git(
        workspace,
        [
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--end-of-options"),
            OsStr::new(&expression),
        ],
    )?;
    let commit = output_line(&output.stdout)?;
    if !(commit.len() == 40 || commit.len() == 64)
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(IsolatedWorktreeError::InvalidRevision);
    }
    Ok(commit.to_ascii_lowercase())
}

fn materialize_commit(
    manager: &IsolatedWorktreeManager,
    source_workspace: &Path,
    revision: &str,
    worktree: &Path,
) -> Result<Inventory, IsolatedWorktreeError> {
    let tree = manager.git(
        source_workspace,
        [
            OsStr::new("ls-tree"),
            OsStr::new("-rz"),
            OsStr::new("--full-tree"),
            OsStr::new(revision),
        ],
    )?;
    let entries = parse_tree(&tree.stdout)?;
    let blobs = manager.read_blobs(source_workspace, &entries)?;
    let mut source_bytes = 0_u64;
    let mut inventory = Sha256::new();
    for (entry, blob) in entries.iter().zip(blobs) {
        let relative = validate_source_path(&entry.path)?;
        let destination = worktree.join(relative);
        if blob.len() > MAX_SOURCE_FILE_BYTES {
            return Err(IsolatedWorktreeError::SourceFileTooLarge(
                entry.path.to_string_lossy().into_owned(),
            ));
        }
        source_bytes = source_bytes
            .checked_add(blob.len() as u64)
            .ok_or(IsolatedWorktreeError::SourceTooLarge)?;
        if source_bytes > MAX_SOURCE_BYTES as u64 {
            return Err(IsolatedWorktreeError::SourceTooLarge);
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.mode {
            0o100644 | 0o100755 => write_regular_file(&destination, &blob, entry.mode)?,
            0o120000 => write_safe_symlink(worktree, &destination, &blob)?,
            _ => {
                return Err(IsolatedWorktreeError::UnsupportedTreeEntry(format!(
                    "{:o} {}",
                    entry.mode,
                    entry.path.display()
                )));
            }
        }
        inventory.update(entry.path.to_string_lossy().as_bytes());
        inventory.update([0]);
        inventory.update(format!("{:o}", entry.mode).as_bytes());
        inventory.update([0]);
        inventory.update(entry.object_id.as_bytes());
        inventory.update([0]);
    }
    Ok(Inventory {
        file_count: entries.len(),
        source_bytes,
        digest: encode_hex(inventory.finalize().as_slice()),
    })
}

fn parse_batch_blobs(
    bytes: &[u8],
    entries: &[TreeEntry],
) -> Result<Vec<Vec<u8>>, IsolatedWorktreeError> {
    let mut cursor = 0_usize;
    let mut blobs = Vec::with_capacity(entries.len());
    for entry in entries {
        let header_end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .and_then(|offset| cursor.checked_add(offset))
            .ok_or_else(|| {
                IsolatedWorktreeError::UnsupportedTreeEntry("truncated cat-file header".into())
            })?;
        let header = std::str::from_utf8(&bytes[cursor..header_end]).map_err(|_| {
            IsolatedWorktreeError::UnsupportedTreeEntry("non-UTF-8 cat-file header".into())
        })?;
        let mut fields = header.split_ascii_whitespace();
        let object_id = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let size = fields
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| IsolatedWorktreeError::UnsupportedTreeEntry(header.into()))?;
        if fields.next().is_some() || object_id != entry.object_id || kind != "blob" {
            return Err(IsolatedWorktreeError::UnsupportedTreeEntry(header.into()));
        }
        if size > MAX_SOURCE_FILE_BYTES {
            return Err(IsolatedWorktreeError::SourceFileTooLarge(
                entry.path.to_string_lossy().into_owned(),
            ));
        }
        let content_start = header_end + 1;
        let content_end = content_start
            .checked_add(size)
            .filter(|end| *end < bytes.len())
            .ok_or_else(|| {
                IsolatedWorktreeError::UnsupportedTreeEntry("truncated cat-file blob".into())
            })?;
        if bytes[content_end] != b'\n' {
            return Err(IsolatedWorktreeError::UnsupportedTreeEntry(
                "invalid cat-file blob terminator".into(),
            ));
        }
        blobs.push(bytes[content_start..content_end].to_vec());
        cursor = content_end + 1;
    }
    if cursor != bytes.len() {
        return Err(IsolatedWorktreeError::UnsupportedTreeEntry(
            "unexpected trailing cat-file output".into(),
        ));
    }
    Ok(blobs)
}

fn parse_tree(bytes: &[u8]) -> Result<Vec<TreeEntry>, IsolatedWorktreeError> {
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if entries.len() == MAX_SOURCE_FILES {
            return Err(IsolatedWorktreeError::TooManyFiles);
        }
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| IsolatedWorktreeError::UnsupportedTreeEntry("missing path".into()))?;
        let header = std::str::from_utf8(&record[..tab])
            .map_err(|_| IsolatedWorktreeError::UnsupportedTreeEntry("non-UTF-8 header".into()))?;
        let path = std::str::from_utf8(&record[tab + 1..])
            .map_err(|_| IsolatedWorktreeError::UnsafeSourcePath("non-UTF-8 path".into()))?;
        let mut fields = header.split_ascii_whitespace();
        let mode_text = fields
            .next()
            .ok_or_else(|| IsolatedWorktreeError::UnsupportedTreeEntry("missing mode".into()))?;
        let kind = fields
            .next()
            .ok_or_else(|| IsolatedWorktreeError::UnsupportedTreeEntry("missing kind".into()))?;
        let object_id = fields.next().ok_or_else(|| {
            IsolatedWorktreeError::UnsupportedTreeEntry("missing object id".into())
        })?;
        if fields.next().is_some() || kind != "blob" {
            return Err(IsolatedWorktreeError::UnsupportedTreeEntry(header.into()));
        }
        let mode = u32::from_str_radix(mode_text, 8)
            .map_err(|_| IsolatedWorktreeError::UnsupportedTreeEntry(header.into()))?;
        if !(object_id.len() == 40 || object_id.len() == 64)
            || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(IsolatedWorktreeError::UnsupportedTreeEntry(header.into()));
        }
        entries.push(TreeEntry {
            mode,
            object_id: object_id.to_ascii_lowercase(),
            path: PathBuf::from(path),
        });
    }
    Ok(entries)
}

fn validate_source_path(path: &Path) -> Result<&Path, IsolatedWorktreeError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(IsolatedWorktreeError::UnsafeSourcePath(
            path.to_string_lossy().into_owned(),
        ));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(IsolatedWorktreeError::UnsafeSourcePath(
                path.to_string_lossy().into_owned(),
            ));
        }
    }
    if path
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name == OsStr::new(".git")))
    {
        return Err(IsolatedWorktreeError::UnsafeSourcePath(
            path.to_string_lossy().into_owned(),
        ));
    }
    Ok(path)
}

fn write_regular_file(
    destination: &Path,
    contents: &[u8],
    mode: u32,
) -> Result<(), IsolatedWorktreeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    file.write_all(contents)?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_safe_symlink(
    worktree: &Path,
    destination: &Path,
    contents: &[u8],
) -> Result<(), IsolatedWorktreeError> {
    use std::os::unix::fs::symlink;

    let target = std::str::from_utf8(contents).map_err(|_| {
        IsolatedWorktreeError::UnsafeSymlink(destination.to_string_lossy().into_owned())
    })?;
    let target_path = Path::new(target);
    if target_path.is_absolute() || target_path.as_os_str().is_empty() {
        return Err(IsolatedWorktreeError::UnsafeSymlink(target.into()));
    }
    let parent = destination.parent().ok_or_else(|| {
        IsolatedWorktreeError::UnsafeSymlink(destination.to_string_lossy().into_owned())
    })?;
    let resolved = lexical_normalize(parent, target_path)
        .ok_or_else(|| IsolatedWorktreeError::UnsafeSymlink(target.into()))?;
    if !resolved.starts_with(worktree) {
        return Err(IsolatedWorktreeError::UnsafeSymlink(target.into()));
    }
    symlink(target_path, destination)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_safe_symlink(
    _worktree: &Path,
    destination: &Path,
    _contents: &[u8],
) -> Result<(), IsolatedWorktreeError> {
    Err(IsolatedWorktreeError::UnsupportedTreeEntry(format!(
        "symlink {}",
        destination.display()
    )))
}

fn lexical_normalize(base: &Path, target: &Path) -> Option<PathBuf> {
    let mut result = base.to_path_buf();
    for component in target.components() {
        match component {
            Component::Normal(value) => result.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(result)
}

fn verify_clean_worktree(
    manager: &IsolatedWorktreeManager,
    worktree: &Path,
) -> Result<(), IsolatedWorktreeError> {
    let output = manager.git(
        worktree,
        [
            OsStr::new("status"),
            OsStr::new("--porcelain=v1"),
            OsStr::new("--untracked-files=all"),
        ],
    )?;
    if output.stdout.is_empty() {
        Ok(())
    } else {
        Err(IsolatedWorktreeError::WorktreeNotClean)
    }
}

fn prepare_private_state_root(path: &Path) -> Result<PathBuf, IsolatedWorktreeError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(IsolatedWorktreeError::InsecureStateRoot(path.to_path_buf()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(IsolatedWorktreeError::InsecureStateRoot(path.to_path_buf()));
            }
        }
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        create_private_directory(path)?;
    }
    fs::canonicalize(path).map_err(IsolatedWorktreeError::Io)
}

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn reject_nested_roots(source: &Path, state: &Path) -> Result<(), IsolatedWorktreeError> {
    if source.starts_with(state) || state.starts_with(source) {
        return Err(IsolatedWorktreeError::NestedRoots);
    }
    Ok(())
}

fn validate_task_id(task_id: &str) -> Result<(), IsolatedWorktreeError> {
    if task_id.is_empty()
        || task_id.len() > MAX_TASK_ID_BYTES
        || !task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(IsolatedWorktreeError::InvalidTaskId);
    }
    Ok(())
}

fn environment_id(task_id: &str, workspace: &Path, revision: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"hyper-term-isolated-worktree-v1\0");
    digest.update(task_id.as_bytes());
    digest.update([0]);
    digest.update(workspace.to_string_lossy().as_bytes());
    digest.update([0]);
    digest.update(revision.as_bytes());
    let encoded = encode_hex(digest.finalize().as_slice());
    format!("{task_id}-{}", &encoded[..16])
}

fn write_manifest(
    path: &Path,
    manifest: &IsolatedWorktreeManifest,
) -> Result<(), IsolatedWorktreeError> {
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_parent(path)?;
    Ok(())
}

fn read_manifest(path: &Path) -> Result<IsolatedWorktreeManifest, IsolatedWorktreeError> {
    let file = File::open(path)?;
    let manifest: IsolatedWorktreeManifest = serde_json::from_reader(file)?;
    if manifest.schema_version != WORKTREE_SCHEMA_VERSION {
        return Err(IsolatedWorktreeError::ManifestMismatch);
    }
    Ok(manifest)
}

fn sync_parent(path: &Path) -> Result<(), IsolatedWorktreeError> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn output_line(bytes: &[u8]) -> Result<&str, IsolatedWorktreeError> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| IsolatedWorktreeError::InvalidRevision)?
        .trim();
    if value.is_empty() || value.lines().count() != 1 {
        return Err(IsolatedWorktreeError::InvalidRevision);
    }
    Ok(value)
}

fn bounded_diagnostic(bytes: &[u8]) -> String {
    const LIMIT: usize = 4096;
    let bounded = &bytes[..bytes.len().min(LIMIT)];
    String::from_utf8_lossy(bounded).trim().to_string()
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn discover_git_executable() -> Result<PathBuf, IsolatedWorktreeError> {
    for candidate in [
        PathBuf::from("/usr/bin/git"),
        PathBuf::from("/opt/homebrew/bin/git"),
        PathBuf::from("/usr/local/bin/git"),
    ] {
        if candidate.is_file() {
            return fs::canonicalize(candidate).map_err(IsolatedWorktreeError::Io);
        }
    }
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let candidate = directory.join("git");
            if candidate.is_file() {
                return fs::canonicalize(candidate).map_err(IsolatedWorktreeError::Io);
            }
        }
    }
    Err(IsolatedWorktreeError::GitUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(cwd: &Path, arguments: &[&str]) -> Output {
        let output = Command::new(discover_git_executable().unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("-C")
            .arg(cwd)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        run_git(&repository, &["init", "-q"]);
        run_git(&repository, &["config", "user.name", "Hyper Term Test"]);
        run_git(
            &repository,
            &["config", "user.email", "hyper-term@example.invalid"],
        );
        fs::create_dir(repository.join("src")).unwrap();
        fs::write(
            repository.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        fs::write(repository.join("src/data.bin"), [0, b'\n', 255, 7]).unwrap();
        fs::write(repository.join("README.md"), "committed\n").unwrap();
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-qm", "fixture"]);
        (temporary, repository)
    }

    #[test]
    fn creates_exact_clean_commit_without_copying_dirty_user_state() {
        let (temporary, repository) = repository();
        fs::write(repository.join("README.md"), "dirty user edit\n").unwrap();
        fs::write(repository.join("untracked-secret.txt"), "do not copy\n").unwrap();
        let filter_marker = temporary.path().join("smudge-ran");
        let hook_marker = temporary.path().join("hook-ran");
        let hooks = temporary.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        fs::write(
            hooks.join("post-checkout"),
            format!("#!/bin/sh\n/usr/bin/touch '{}'\n", hook_marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                hooks.join("post-checkout"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        run_git(
            &repository,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        run_git(
            &repository,
            &[
                "config",
                "filter.hyper-term-test.smudge",
                &format!("/usr/bin/touch '{}'", filter_marker.display()),
            ],
        );
        fs::write(
            repository.join(".gitattributes"),
            "*.md filter=hyper-term-test\n",
        )
        .unwrap();
        run_git(&repository, &["add", ".gitattributes"]);
        run_git(&repository, &["commit", "-qm", "hostile checkout config"]);
        fs::write(repository.join("README.md"), "dirty user edit\n").unwrap();
        let state_root = temporary.path().join("private-state");
        let manager = IsolatedWorktreeManager::discover().unwrap();
        let environment = manager
            .create(&IsolatedWorktreeRequest {
                source_workspace: repository.clone(),
                state_root,
                task_id: "task-17".into(),
                revision: None,
            })
            .unwrap();

        assert_eq!(
            fs::read_to_string(environment.manifest.worktree.join("README.md")).unwrap(),
            "committed\n"
        );
        assert!(
            !environment
                .manifest
                .worktree
                .join("untracked-secret.txt")
                .exists()
        );
        assert_eq!(environment.manifest.file_count, 4);
        assert_eq!(
            fs::read(environment.manifest.worktree.join("src/data.bin")).unwrap(),
            [0, b'\n', 255, 7]
        );
        assert_eq!(environment.manifest.inventory_sha256.len(), 64);
        assert!(!filter_marker.exists());
        assert!(!hook_marker.exists());
        assert!(
            run_git(
                &environment.manifest.worktree,
                &["status", "--porcelain=v1"]
            )
            .stdout
            .is_empty()
        );

        fs::write(
            environment.manifest.worktree.join("README.md"),
            "isolated edit\n",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(repository.join("README.md")).unwrap(),
            "dirty user edit\n"
        );
        let worktree = environment.manifest.worktree.clone();
        manager.destroy(&environment).unwrap();
        assert!(!worktree.exists());
        assert_eq!(
            run_git(&repository, &["worktree", "list", "--porcelain"])
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| line.starts_with(b"worktree "))
                .count(),
            1
        );
        assert_eq!(
            fs::read_to_string(repository.join("README.md")).unwrap(),
            "dirty user edit\n"
        );
    }

    #[test]
    fn manifest_is_private_and_required_for_cleanup() {
        let (temporary, repository) = repository();
        let manager = IsolatedWorktreeManager::discover().unwrap();
        let environment = manager
            .create(&IsolatedWorktreeRequest {
                source_workspace: repository,
                state_root: temporary.path().join("state"),
                task_id: "private".into(),
                revision: Some("HEAD".into()),
            })
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&environment.manifest_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::write(&environment.manifest_path, "{}\n").unwrap();
        assert!(matches!(
            manager.destroy(&environment),
            Err(IsolatedWorktreeError::InvalidManifest(_))
        ));
    }

    #[test]
    fn rejects_nested_state_root_and_invalid_task_identity() {
        let (_temporary, repository) = repository();
        let manager = IsolatedWorktreeManager::discover().unwrap();
        let nested = manager.create(&IsolatedWorktreeRequest {
            source_workspace: repository.clone(),
            state_root: repository.join(".hyper-term/tasks"),
            task_id: "task".into(),
            revision: None,
        });
        assert!(matches!(nested, Err(IsolatedWorktreeError::NestedRoots)));
        let invalid = manager.create(&IsolatedWorktreeRequest {
            source_workspace: repository.clone(),
            state_root: repository.parent().unwrap().join("state"),
            task_id: "../../escape".into(),
            revision: None,
        });
        assert!(matches!(invalid, Err(IsolatedWorktreeError::InvalidTaskId)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_an_existing_state_root_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let (temporary, repository) = repository();
        let state_root = temporary.path().join("shared-state");
        fs::create_dir(&state_root).unwrap();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o755)).unwrap();
        let manager = IsolatedWorktreeManager::discover().unwrap();
        let result = manager.create(&IsolatedWorktreeRequest {
            source_workspace: repository,
            state_root: state_root.clone(),
            task_id: "private-state".into(),
            revision: None,
        });
        assert!(matches!(
            result,
            Err(IsolatedWorktreeError::InsecureStateRoot(path)) if path == state_root
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_the_isolated_worktree() {
        use std::os::unix::fs::symlink;

        let (temporary, repository) = repository();
        symlink("../../outside", repository.join("escape")).unwrap();
        run_git(&repository, &["add", "escape"]);
        run_git(&repository, &["commit", "-qm", "escaping symlink"]);
        let state_root = temporary.path().join("state");
        let manager = IsolatedWorktreeManager::discover().unwrap();
        let result = manager.create(&IsolatedWorktreeRequest {
            source_workspace: repository.clone(),
            state_root,
            task_id: "unsafe-link".into(),
            revision: None,
        });
        assert!(matches!(
            result,
            Err(IsolatedWorktreeError::UnsafeSymlink(_))
        ));
        assert!(
            run_git(&repository, &["worktree", "list", "--porcelain"])
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| line.starts_with(b"worktree "))
                .count()
                == 1
        );
    }
}
