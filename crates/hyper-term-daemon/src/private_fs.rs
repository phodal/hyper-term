use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory(path, &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error),
    }

    let metadata = fs::symlink_metadata(path)?;
    validate_private_directory(path, &metadata)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn validate_private_directory(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "private state path is not a regular directory: {}",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private state directory has a different owner: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[test]
    #[cfg(unix)]
    fn private_directory_migrates_mode_and_rejects_a_symbolic_link() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = temporary.path().join("state");
        fs::create_dir(&directory).expect("state directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .expect("public fixture mode");

        ensure_private_directory(&directory).expect("private directory");
        assert_eq!(
            fs::symlink_metadata(&directory)
                .expect("private metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let redirected = temporary.path().join("redirected");
        symlink(&directory, &redirected).expect("directory symlink");
        assert_eq!(
            ensure_private_directory(&redirected)
                .expect_err("symlink must be rejected")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
