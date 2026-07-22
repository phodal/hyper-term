use std::io::{Error, ErrorKind, Read as _};

const MAX_CLAUDE_KEYCHAIN_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) fn sanitize_claude_credentials(source: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let source: serde_json::Value = serde_json::from_slice(source)
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    let oauth = source
        .get("claudeAiOauth")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "Claude credential does not contain an OAuth record",
            )
        })?;
    if oauth
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Claude OAuth record does not contain an access token",
        ));
    }
    serde_json::to_vec(&serde_json::json!({ "claudeAiOauth": oauth }))
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))
}

#[cfg(target_os = "macos")]
pub(crate) fn read_claude_keychain_credentials() -> Result<Option<Vec<u8>>, std::io::Error> {
    use std::process::{Command, Stdio};

    const SECURITY_EXECUTABLE: &str = "/usr/bin/security";
    const CLAUDE_CREDENTIAL_SERVICE: &str = "Claude Code-credentials";
    const SECURITY_ITEM_NOT_FOUND: i32 = 44;

    let mut child = Command::new(SECURITY_EXECUTABLE)
        .args([
            "find-generic-password",
            "-s",
            CLAUDE_CREDENTIAL_SERVICE,
            "-w",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("Claude Keychain reader has no stdout pipe"))?;
    let mut bytes = Vec::new();
    if let Err(error) = stdout
        .by_ref()
        .take(MAX_CLAUDE_KEYCHAIN_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        let _ = child.kill();
        let _ = child.wait();
        bytes.fill(0);
        return Err(error);
    }
    if bytes.len() as u64 > MAX_CLAUDE_KEYCHAIN_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        bytes.fill(0);
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Claude Keychain credential exceeds the bounded launch size",
        ));
    }
    let status = child.wait()?;
    if status.success() {
        let sanitized = sanitize_claude_credentials(&bytes);
        bytes.fill(0);
        return sanitized.map(Some);
    }
    bytes.fill(0);
    if status.code() == Some(SECURITY_ITEM_NOT_FOUND) {
        Ok(None)
    } else {
        Err(Error::other("Claude Keychain credential lookup failed"))
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read_claude_keychain_credentials() -> Result<Option<Vec<u8>>, std::io::Error> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_keeps_only_claude_oauth_credentials() {
        let sanitized = sanitize_claude_credentials(
            br#"{
                "claudeAiOauth": {
                    "accessToken": "secret-access",
                    "refreshToken": "secret-refresh",
                    "expiresAt": 42
                },
                "mcpOAuth": {"unsafe": "other-authority"}
            }"#,
        )
        .unwrap();
        let sanitized: serde_json::Value = serde_json::from_slice(&sanitized).unwrap();
        assert_eq!(sanitized["claudeAiOauth"]["accessToken"], "secret-access");
        assert_eq!(sanitized["claudeAiOauth"]["refreshToken"], "secret-refresh");
        assert!(sanitized.get("mcpOAuth").is_none());
    }

    #[test]
    fn sanitizer_rejects_records_without_an_access_token() {
        let error =
            sanitize_claude_credentials(br#"{"claudeAiOauth":{"expiresAt":42}}"#).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }
}
