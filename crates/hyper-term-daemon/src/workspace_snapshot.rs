use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use thiserror::Error;

const MAX_SNAPSHOT_FILES: usize = 16 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 96 * 1024 * 1024;
const MAX_SNAPSHOT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SNAPSHOT_DEPTH: usize = 32;
const MAX_RELATIVE_PATH_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSnapshot {
    pub root: PathBuf,
}

pub(crate) fn create_workspace_snapshot(
    source: &Path,
    destination: &Path,
) -> Result<WorkspaceSnapshot, WorkspaceSnapshotError> {
    if !source.is_absolute() || !destination.is_absolute() {
        return Err(WorkspaceSnapshotError::AbsolutePathsRequired);
    }
    let source = source.canonicalize().map_err(WorkspaceSnapshotError::Io)?;
    if !source.is_dir() {
        return Err(WorkspaceSnapshotError::SourceIsNotDirectory);
    }
    if destination.exists() {
        return Err(WorkspaceSnapshotError::DestinationExists);
    }
    create_private_directory(destination)?;
    let destination = destination.canonicalize()?;
    let result = capture_snapshot(&source, &destination);
    if result.is_err() {
        let _ = fs::remove_dir_all(&destination);
    }
    result
}

fn capture_snapshot(
    source: &Path,
    destination: &Path,
) -> Result<WorkspaceSnapshot, WorkspaceSnapshotError> {
    let mut pending = vec![(source.to_owned(), PathBuf::new(), 0_usize)];
    let mut file_count = 0_usize;
    let mut total_bytes = 0_u64;

    while let Some((directory, relative, depth)) = pending.pop() {
        if depth > MAX_SNAPSHOT_DEPTH {
            return Err(WorkspaceSnapshotError::DepthLimit);
        }
        let canonical_directory = directory.canonicalize()?;
        if !canonical_directory.starts_with(source) {
            return Err(WorkspaceSnapshotError::SourceEscaped);
        }
        let mut entries = fs::read_dir(&canonical_directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        entries.reverse();

        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let relative_path = relative.join(&name);
            validate_relative_path(&relative_path)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }

            if metadata.is_dir() {
                if excluded_directory(&name) {
                    continue;
                }
                let canonical = path.canonicalize()?;
                if !canonical.starts_with(source)
                    || canonical.starts_with(destination)
                    || destination.starts_with(&canonical)
                {
                    continue;
                }
                create_private_directory(&destination.join(&relative_path))?;
                pending.push((canonical, relative_path, depth + 1));
                continue;
            }

            if !metadata.is_file() || !included_source_file(&name) {
                continue;
            }
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(source) || canonical.starts_with(destination) {
                return Err(WorkspaceSnapshotError::SourceEscaped);
            }
            let Some(bytes) = read_bounded_regular_file(&canonical)? else {
                continue;
            };
            let next_file_count = file_count
                .checked_add(1)
                .ok_or(WorkspaceSnapshotError::FileLimit)?;
            if next_file_count > MAX_SNAPSHOT_FILES {
                return Err(WorkspaceSnapshotError::FileLimit);
            }
            let next_total_bytes = total_bytes
                .checked_add(bytes.len() as u64)
                .ok_or(WorkspaceSnapshotError::ByteLimit)?;
            if next_total_bytes > MAX_SNAPSHOT_BYTES {
                return Err(WorkspaceSnapshotError::ByteLimit);
            }

            write_read_only_file(&destination.join(&relative_path), &bytes)?;
            file_count = next_file_count;
            total_bytes = next_total_bytes;
        }
    }

    Ok(WorkspaceSnapshot {
        root: destination.to_owned(),
    })
}

fn validate_relative_path(path: &Path) -> Result<(), WorkspaceSnapshotError> {
    let Some(text) = path.to_str() else {
        return Err(WorkspaceSnapshotError::InvalidRelativePath);
    };
    if text.is_empty()
        || text.len() > MAX_RELATIVE_PATH_BYTES
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceSnapshotError::InvalidRelativePath);
    }
    Ok(())
}

fn excluded_directory(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git"
            | ".hyper-term"
            | ".native"
            | ".next"
            | ".turbo"
            | ".venv"
            | ".zig-cache"
            | "build"
            | "coverage"
            | "dist"
            | "node_modules"
            | "out"
            | "target"
            | "venv"
            | "zig-out"
    )
}

fn included_source_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        Path::new(&lower)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some(
            "cjs"
                | "css"
                | "cts"
                | "html"
                | "js"
                | "json"
                | "jsonc"
                | "jsx"
                | "less"
                | "md"
                | "mjs"
                | "mts"
                | "scss"
                | "ts"
                | "tsx"
        )
    ) || matches!(lower.as_str(), "deno.lock" | "import_map")
}

fn read_bounded_regular_file(path: &Path) -> Result<Option<Vec<u8>>, WorkspaceSnapshotError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(WorkspaceSnapshotError::SourceEscaped);
    }
    if metadata.len() > MAX_SNAPSHOT_FILE_BYTES {
        return Err(WorkspaceSnapshotError::SingleFileLimit(path.to_owned()));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SNAPSHOT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_FILE_BYTES {
        return Err(WorkspaceSnapshotError::SingleFileLimit(path.to_owned()));
    }
    if bytes.iter().take(8 * 1024).any(|byte| *byte == 0) {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    crate::private_fs::ensure_private_directory(path)
}

pub(crate) fn create_private_runtime_root(path: &Path) -> Result<(), std::io::Error> {
    create_private_directory(path)
}

fn write_read_only_file(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("snapshot file has no parent"))?;
    create_private_directory(parent)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o400))?;
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum WorkspaceSnapshotError {
    #[error("workspace snapshot source and destination must be absolute")]
    AbsolutePathsRequired,
    #[error("workspace snapshot source is not a directory")]
    SourceIsNotDirectory,
    #[error("workspace snapshot destination already exists")]
    DestinationExists,
    #[error("workspace snapshot source escaped its canonical root")]
    SourceEscaped,
    #[error("workspace snapshot contains an invalid relative path")]
    InvalidRelativePath,
    #[error("workspace snapshot exceeded its file count limit")]
    FileLimit,
    #[error("workspace snapshot exceeded its total byte limit")]
    ByteLimit,
    #[error("workspace snapshot exceeded its directory depth limit")]
    DepthLimit,
    #[error("workspace snapshot file exceeds its byte limit: {0}")]
    SingleFileLimit(PathBuf),
    #[error("workspace snapshot I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_private_bounded_text_and_skips_generated_or_linked_content() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let destination = temporary.path().join("state/session/snapshot");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir_all(workspace.join("node_modules/pkg")).unwrap();
        fs::write(
            workspace.join("src/App.tsx"),
            "export default () => <main />;\n",
        )
        .unwrap();
        fs::write(workspace.join("package.json"), "{\"type\":\"module\"}\n").unwrap();
        fs::write(workspace.join("image.png"), [0_u8, 1, 2]).unwrap();
        fs::write(workspace.join("node_modules/pkg/index.ts"), "export {};").unwrap();
        let outside = temporary.path().join("outside.ts");
        fs::write(&outside, "export const secret = true;").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, workspace.join("src/linked.ts")).unwrap();

        let snapshot =
            create_workspace_snapshot(&workspace.canonicalize().unwrap(), &destination).unwrap();

        assert_eq!(
            fs::read_to_string(snapshot.root.join("src/App.tsx")).unwrap(),
            "export default () => <main />;\n"
        );
        assert!(!snapshot.root.join("image.png").exists());
        assert!(!snapshot.root.join("node_modules").exists());
        assert!(!snapshot.root.join("src/linked.ts").exists());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(snapshot.root.join("src/App.tsx"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
    }

    #[test]
    fn oversized_source_fails_closed_and_removes_the_partial_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let destination = temporary.path().join("snapshot");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join("large.ts"),
            vec![b'x'; MAX_SNAPSHOT_FILE_BYTES as usize + 1],
        )
        .unwrap();

        assert!(matches!(
            create_workspace_snapshot(&workspace.canonicalize().unwrap(), &destination),
            Err(WorkspaceSnapshotError::SingleFileLimit(_))
        ));
        assert!(!destination.exists());
    }

    #[test]
    fn destination_nested_inside_source_is_never_recaptured() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/main.ts"), "export const value = 1;\n").unwrap();
        let destination = workspace.join("private-state/session/snapshot");

        let snapshot =
            create_workspace_snapshot(&workspace.canonicalize().unwrap(), &destination).unwrap();

        assert!(snapshot.root.join("src/main.ts").is_file());
        assert!(!snapshot.root.join("private-state").exists());
    }
}
