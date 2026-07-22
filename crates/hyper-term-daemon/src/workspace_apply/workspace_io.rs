use super::*;

pub(super) fn validate_target_path(value: &str) -> Result<PathBuf, WorkspaceApplyError> {
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

pub(super) fn open_parent(
    workspace: &Path,
    relative: &Path,
) -> Result<OpenParent, WorkspaceApplyError> {
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

pub(super) fn read_target_at(
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
    let digest = sha256_bytes(&bytes);
    let (content, binary_bytes) = match String::from_utf8(bytes) {
        Ok(content) => (content, None),
        Err(error) => (String::new(), Some(error.as_bytes().len() as u64)),
    };
    Ok(Some(WorkspaceFileSnapshot {
        digest,
        content,
        binary_bytes,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
    }))
}

pub(super) fn create_stage(
    parent_fd: RawFd,
    stage_name: &CStr,
) -> Result<File, WorkspaceApplyError> {
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

#[cfg(all(test, target_os = "macos"))]
pub(super) fn install_stage(
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

pub(super) fn link_target_at(
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
pub(super) fn install_transaction_stage(
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
pub(super) fn install_transaction_stage(
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

pub(super) fn replace_target_at(
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

#[cfg(all(test, not(target_os = "macos")))]
pub(super) fn install_stage(
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

pub(super) fn same_file_state(
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

pub(super) fn c_string(value: &OsStr) -> Result<CString, WorkspaceApplyError> {
    CString::new(value.as_bytes()).map_err(|_| WorkspaceApplyError::InvalidPath)
}

pub(super) fn unlink_at(parent_fd: RawFd, name: &CStr) {
    // SAFETY: name is relative to an open directory descriptor; cleanup is best effort.
    let _ = unsafe { libc::unlinkat(parent_fd, name.as_ptr(), 0) };
}

pub(super) fn unlink_at_checked(parent_fd: RawFd, name: &CStr) -> Result<(), WorkspaceApplyError> {
    // SAFETY: name is bounded and relative to an open directory descriptor.
    let result = unsafe { libc::unlinkat(parent_fd, name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(WorkspaceApplyError::Io(std::io::Error::last_os_error()))
    }
}

pub(super) fn unlink_at_if_exists(
    parent_fd: RawFd,
    name: &CStr,
) -> Result<(), WorkspaceApplyError> {
    // SAFETY: name is bounded and relative to an open directory descriptor.
    let result = unsafe { libc::unlinkat(parent_fd, name.as_ptr(), 0) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(WorkspaceApplyError::Io(error))
    }
}

pub(super) fn encode_workspace_content(bytes: &[u8]) -> (String, Option<String>) {
    match std::str::from_utf8(bytes) {
        Ok(content) => (content.to_owned(), None),
        Err(_) => (String::new(), Some(BASE64_STANDARD.encode(bytes))),
    }
}

pub(super) fn decoded_base64_len(encoded: &str) -> Result<usize, WorkspaceApplyError> {
    BASE64_STANDARD
        .decode(encoded)
        .map(|bytes| bytes.len())
        .map_err(|_| WorkspaceApplyError::InvalidPath)
}

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    sha256_digest(Sha256::digest(bytes))
}

pub(super) fn sha256_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn is_false(value: &bool) -> bool {
    !*value
}
