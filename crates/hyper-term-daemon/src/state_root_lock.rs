use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::DaemonError;

pub(crate) const DAEMON_STATE_LOCK_FILE: &str = "daemon.lock";

pub(crate) struct StateRootLock {
    _file: File,
}

impl StateRootLock {
    pub(crate) fn acquire(state_directory: &Path) -> Result<Self, DaemonError> {
        let path = state_directory.join(DAEMON_STATE_LOCK_FILE);
        reject_symlink(&path)?;
        let file = open_lock_file(&path)?;
        make_private(&file)?;
        try_lock(&file).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                DaemonError::StateDirectoryInUse(state_directory.to_path_buf())
            } else {
                DaemonError::Io(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

fn reject_symlink(path: &Path) -> Result<(), DaemonError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(DaemonError::InvalidStateLock(path.to_path_buf()))
        }
        Ok(metadata) if !metadata.is_file() => {
            Err(DaemonError::InvalidStateLock(path.to_path_buf()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_lock_file(path: &Path) -> Result<File, DaemonError> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?)
}

fn make_private(file: &File) -> Result<(), DaemonError> {
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn try_lock(file: &File) -> io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn lock_descriptor_is_not_inherited_by_exec() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let lock = StateRootLock::acquire(temporary.path()).expect("state lock");
        let flags = unsafe { libc::fcntl(lock._file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn lock_path_cannot_redirect_through_a_symbolic_link() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("outside");
        File::create(&target).expect("target file");
        let lock_path = temporary.path().join(DAEMON_STATE_LOCK_FILE);
        symlink(&target, &lock_path).expect("lock symlink");

        assert!(matches!(
            StateRootLock::acquire(temporary.path()),
            Err(DaemonError::InvalidStateLock(path)) if path == lock_path
        ));
    }
}
