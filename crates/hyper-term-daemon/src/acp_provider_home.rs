use std::path::Path;

use crate::workspace_snapshot::create_private_runtime_root;

const MAX_CLAUDE_AUTH_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_COPILOT_CONFIG_BYTES: u64 = 1024 * 1024;
const COPILOT_CONFIG_FILES: &[&str] = &[
    "config.json",
    "settings.json",
    "permissions-config.json",
    "mcp-config.json",
];

#[cfg(unix)]
pub(crate) fn stage_acp_codex_preferences(
    home: &Path,
    codex_home: &Path,
) -> Result<(), std::io::Error> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::symlink;

    require_absolute_homes(home, codex_home, "ACP Codex")?;
    let source_root = home.join(".codex");
    let Ok(canonical_root) = source_root.canonicalize() else {
        return Ok(());
    };
    for relative in ["config.toml", "AGENTS.md"] {
        let source = source_root.join(relative);
        let Ok(metadata) = std::fs::symlink_metadata(&source) else {
            continue;
        };
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ACP Codex preference source is not a regular file or directory",
            ));
        }
        let canonical = source.canonicalize()?;
        if !canonical.starts_with(&canonical_root) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ACP Codex preference source escaped its root",
            ));
        }
        let target = codex_home.join(relative);
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "ACP Codex preference target already exists",
            ));
        }
        symlink(canonical, target)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn stage_acp_codex_preferences(
    _home: &Path,
    _codex_home: &Path,
) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn stage_acp_claude_home(
    home: &Path,
    isolated_home: &Path,
) -> Result<(), std::io::Error> {
    use std::fs::OpenOptions;
    use std::io::{Error, ErrorKind, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};

    require_absolute_homes(home, isolated_home, "ACP Claude")?;
    let claude_home = isolated_home.join(".claude");
    create_private_runtime_root(&claude_home)?;

    let source_metadata_path = home.join(".claude.json");
    if let Ok(source_metadata) = std::fs::symlink_metadata(&source_metadata_path) {
        if source_metadata.file_type().is_symlink()
            || !source_metadata.is_file()
            || source_metadata.uid() != unsafe { libc::geteuid() }
            || source_metadata.permissions().mode() & 0o077 != 0
            || source_metadata.len() > MAX_CLAUDE_AUTH_METADATA_BYTES
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ACP Claude auth metadata is not a bounded private user file",
            ));
        }
        let source = std::fs::read(&source_metadata_path)?;
        let source: serde_json::Value = serde_json::from_slice(&source)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let mut staged = serde_json::Map::new();
        if let Some(account) = source
            .get("oauthAccount")
            .filter(|account| account.is_object())
        {
            staged.insert("oauthAccount".into(), account.clone());
        }
        let staged = serde_json::to_vec(&serde_json::Value::Object(staged))
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let target = isolated_home.join(".claude.json");
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?;
        target.write_all(&staged)?;
        target.sync_all()?;
    }

    let source_claude_home = home.join(".claude");
    if let Ok(canonical_root) = source_claude_home.canonicalize() {
        for relative in [
            "settings.json",
            "settings.local.json",
            "CLAUDE.md",
            "skills",
        ] {
            let source = source_claude_home.join(relative);
            let Ok(metadata) = std::fs::symlink_metadata(&source) else {
                continue;
            };
            if metadata.file_type().is_symlink()
                || (!metadata.is_file() && !metadata.is_dir())
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "ACP Claude preference source is unsafe",
                ));
            }
            let canonical = source.canonicalize()?;
            if !canonical.starts_with(&canonical_root) {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "ACP Claude preference source escaped its root",
                ));
            }
            symlink(canonical, claude_home.join(relative))?;
        }

        let credentials = source_claude_home.join(".credentials.json");
        if let Ok(metadata) = std::fs::symlink_metadata(&credentials) {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.len() > MAX_CLAUDE_AUTH_METADATA_BYTES
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "ACP Claude credential file is unsafe",
                ));
            }
            copy_private_file(&credentials, &claude_home.join(".credentials.json"))?;
        }
    }

    stage_keychains(home, isolated_home, "ACP Claude")
}

#[cfg(not(unix))]
pub(crate) fn stage_acp_claude_home(
    _home: &Path,
    _isolated_home: &Path,
) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn stage_acp_copilot_home(
    home: &Path,
    isolated_home: &Path,
) -> Result<(), std::io::Error> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::MetadataExt;

    require_absolute_homes(home, isolated_home, "ACP Copilot")?;
    let source_root = home.join(".copilot");
    let copilot_home = isolated_home.join(".copilot");
    create_private_runtime_root(&copilot_home)?;

    if let Ok(canonical_root) = source_root.canonicalize() {
        for relative in COPILOT_CONFIG_FILES {
            let source = source_root.join(relative);
            let Ok(metadata) = std::fs::symlink_metadata(&source) else {
                continue;
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.len() > MAX_COPILOT_CONFIG_BYTES
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "ACP Copilot configuration source is unsafe",
                ));
            }
            let canonical = source.canonicalize()?;
            if !canonical.starts_with(&canonical_root) {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "ACP Copilot configuration source escaped its root",
                ));
            }
            copy_private_file(&canonical, &copilot_home.join(relative))?;
        }
    }

    stage_keychains(home, isolated_home, "ACP Copilot")
}

#[cfg(not(unix))]
pub(crate) fn stage_acp_copilot_home(
    _home: &Path,
    _isolated_home: &Path,
) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn require_absolute_homes(
    home: &Path,
    isolated_home: &Path,
    provider: &str,
) -> Result<(), std::io::Error> {
    if home.is_absolute() && isolated_home.is_absolute() {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{provider} homes must be absolute"),
    ))
}

#[cfg(unix)]
fn copy_private_file(source: &Path, target: &Path) -> Result<(), std::io::Error> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)?;
    target.write_all(&std::fs::read(source)?)?;
    target.sync_all()
}

#[cfg(unix)]
fn stage_keychains(
    home: &Path,
    isolated_home: &Path,
    provider: &str,
) -> Result<(), std::io::Error> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::{MetadataExt, symlink};

    let library = isolated_home.join("Library");
    create_private_runtime_root(&library)?;
    let keychains = home.join("Library/Keychains");
    let Ok(metadata) = std::fs::symlink_metadata(&keychains) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("{provider} Keychain source is unsafe"),
        ));
    }
    symlink(keychains.canonicalize()?, library.join("Keychains"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn claude_home_stages_only_auth_metadata_and_read_only_preferences() {
        let root = TempDir::new().unwrap();
        let home = root.path().join("provider-home");
        let source_config = home.join(".claude");
        let keychains = home.join("Library/Keychains");
        fs::create_dir_all(source_config.join("skills")).unwrap();
        fs::create_dir_all(&keychains).unwrap();
        let source_metadata = home.join(".claude.json");
        fs::write(
            &source_metadata,
            r#"{"oauthAccount":{"accountUuid":"fixture"},"mcpServers":{"unsafe":{}}}"#,
        )
        .unwrap();
        fs::set_permissions(&source_metadata, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(source_config.join("settings.json"), "{}").unwrap();
        let credentials = source_config.join(".credentials.json");
        fs::write(
            &credentials,
            r#"{"claudeAiOauth":{"accessToken":"fixture"}}"#,
        )
        .unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();

        let isolated_home = root.path().join("runtime/home");
        stage_acp_claude_home(&home, &isolated_home).unwrap();

        let staged: serde_json::Value =
            serde_json::from_slice(&fs::read(isolated_home.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(staged["oauthAccount"]["accountUuid"], "fixture");
        assert!(staged.get("mcpServers").is_none());
        assert_eq!(
            fs::metadata(isolated_home.join(".claude.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert!(
            fs::symlink_metadata(isolated_home.join(".claude/settings.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(isolated_home.join(".claude/skills"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            !fs::symlink_metadata(isolated_home.join(".claude/.credentials.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(isolated_home.join(".claude/.credentials.json")).unwrap(),
            fs::read(credentials).unwrap()
        );
        assert!(
            fs::symlink_metadata(isolated_home.join("Library/Keychains"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn copilot_home_stages_bounded_configuration_without_history() {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let isolated = root.path().join("isolated");
        fs::create_dir_all(home.join(".copilot/session-state")).unwrap();
        fs::create_dir_all(home.join("Library/Keychains")).unwrap();
        fs::create_dir_all(&isolated).unwrap();
        fs::write(home.join(".copilot/settings.json"), b"{\"model\":\"auto\"}").unwrap();
        fs::write(home.join(".copilot/session-state/private.json"), b"history").unwrap();

        stage_acp_copilot_home(&home, &isolated).unwrap();

        assert_eq!(
            fs::read(isolated.join(".copilot/settings.json")).unwrap(),
            b"{\"model\":\"auto\"}"
        );
        assert!(!isolated.join(".copilot/session-state").exists());
        assert!(
            fs::symlink_metadata(isolated.join("Library/Keychains"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(isolated.join(".copilot/settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }

    #[test]
    fn copilot_home_rejects_symlinked_configuration() {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let isolated = root.path().join("isolated");
        fs::create_dir_all(home.join(".copilot")).unwrap();
        fs::create_dir_all(&isolated).unwrap();
        fs::write(root.path().join("outside.json"), b"{}").unwrap();
        symlink(
            root.path().join("outside.json"),
            home.join(".copilot/settings.json"),
        )
        .unwrap();

        let error = stage_acp_copilot_home(&home, &isolated).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
