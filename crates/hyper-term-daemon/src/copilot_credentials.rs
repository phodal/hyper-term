use std::ffi::{OsStr, OsString};
use std::io::{Error, ErrorKind, Read as _};
use std::path::{Path, PathBuf};

const MAX_GITHUB_TOKEN_BYTES: u64 = 64 * 1024;

#[cfg(unix)]
fn take_valid_token(bytes: &mut Vec<u8>) -> Result<OsString, std::io::Error> {
    use std::os::unix::ffi::OsStringExt;

    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    let token = &bytes[start..end];
    if token.is_empty() || !token.iter().all(|byte| (0x21..=0x7e).contains(byte)) {
        bytes.fill(0);
        return Err(Error::new(
            ErrorKind::InvalidData,
            "GitHub CLI returned an invalid authentication token",
        ));
    }
    let token = OsString::from_vec(token.to_vec());
    bytes.fill(0);
    Ok(token)
}

#[cfg(unix)]
fn resolve_gh_executable(path: Option<&OsStr>) -> Option<PathBuf> {
    path.into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("gh"))
        .find_map(|candidate| {
            let canonical = candidate.canonicalize().ok()?;
            canonical.is_file().then_some(canonical)
        })
}

#[cfg(unix)]
pub(crate) fn read_github_cli_token(
    home: &Path,
    path: Option<&OsStr>,
) -> Result<Option<OsString>, std::io::Error> {
    use std::process::{Command, Stdio};

    if !home.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "GitHub CLI credential home must be absolute",
        ));
    }
    let Some(executable) = resolve_gh_executable(path) else {
        return Ok(None);
    };
    let mut child = Command::new(executable)
        .args(["auth", "token", "--hostname", "github.com"])
        .env_clear()
        .env("HOME", home)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("GitHub CLI credential reader has no stdout pipe"))?;
    let mut bytes = Vec::new();
    if let Err(error) = stdout
        .by_ref()
        .take(MAX_GITHUB_TOKEN_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        let _ = child.kill();
        let _ = child.wait();
        bytes.fill(0);
        return Err(error);
    }
    if bytes.len() as u64 > MAX_GITHUB_TOKEN_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        bytes.fill(0);
        return Err(Error::new(
            ErrorKind::InvalidData,
            "GitHub CLI credential exceeds the bounded launch size",
        ));
    }
    let status = child.wait()?;
    if !status.success() {
        bytes.fill(0);
        return Ok(None);
    }
    take_valid_token(&mut bytes).map(Some)
}

#[cfg(not(unix))]
pub(crate) fn read_github_cli_token(
    _home: &Path,
    _path: Option<&OsStr>,
) -> Result<Option<OsString>, std::io::Error> {
    Ok(None)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn token_is_trimmed_and_the_capture_buffer_is_zeroed() {
        let mut bytes = b"  github_pat_fixture\n".to_vec();
        let token = take_valid_token(&mut bytes).unwrap();
        assert_eq!(token, "github_pat_fixture");
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn token_rejects_controls_without_retaining_the_capture() {
        let mut bytes = b"github_pat_fixture\nembedded".to_vec();
        assert_eq!(
            take_valid_token(&mut bytes).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}
