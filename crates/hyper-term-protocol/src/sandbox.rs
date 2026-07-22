use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{Actor, OperationId, SandboxLeaseId};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DigestError {
    #[error("a SHA-256 digest must contain exactly 64 lowercase hexadecimal characters")]
    InvalidSha256,
}

macro_rules! sha256_digest {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, DigestError> {
                let value = value.into();
                if value.len() != 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(DigestError::InvalidSha256);
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(de::Error::custom)
            }
        }
    };
}

sha256_digest!(ActionDigest);
sha256_digest!(ApprovalDetailDigest);
sha256_digest!(ContextDigest);
sha256_digest!(EnvironmentPlanDigest);
sha256_digest!(McpArgumentsDigest);
sha256_digest!(McpCapabilitiesDigest);
sha256_digest!(McpCatalogDigest);
sha256_digest!(McpRuntimeIdentityDigest);
sha256_digest!(McpToolContractDigest);
sha256_digest!(McpToolResultDigest);
sha256_digest!(SandboxProfileDigest);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEnforcement {
    Native,
    IsolatedTask,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackendKind {
    MacOsSeatbelt,
    LimaVm,
    LinuxBubblewrap,
    WindowsRestrictedToken,
    TestOnlyUnenforced,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPathAccess {
    Read,
    Write,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxPathRule {
    pub path: PathBuf,
    pub access: SandboxPathAccess,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxFileSystemPolicy {
    #[serde(default)]
    pub rules: Vec<SandboxPathRule>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SandboxNetworkPolicy {
    Offline,
    ProxyOnly {
        proxy_url: String,
        #[serde(default)]
        allowed_hosts: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allowed_unix_sockets: Vec<PathBuf>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxEnvironmentPolicy {
    pub clear_inherited: bool,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
}

impl Default for SandboxEnvironmentPolicy {
    fn default() -> Self {
        Self {
            clear_inherited: true,
            variables: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxProcessPolicy {
    pub allow_child_processes: bool,
    /// Allows any executable already visible through the filesystem policy.
    /// This is required for user-approved shell scripts whose child commands
    /// cannot be reduced to a static executable list before execution.
    #[serde(default)]
    pub allow_any_executable: bool,
    #[serde(default)]
    pub allowed_executables: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxResourceLimits {
    pub wall_time_ms: Option<u64>,
    pub max_processes: Option<u32>,
    pub max_output_bytes: Option<u64>,
}

/// Platform capabilities that cannot be expressed through portable
/// filesystem, network, environment, or process rules.
///
/// These capabilities are deny-by-default and must be selected by Rust for an
/// exact provider. Presentation code cannot add them at launch time.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxPlatformPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macos_mach_services: Vec<String>,
}

impl SandboxPlatformPolicy {
    pub fn is_empty(&self) -> bool {
        self.macos_mach_services.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxLifetime {
    OneOperation,
    OneTask,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxProfile {
    pub enforcement: SandboxEnforcement,
    pub filesystem: SandboxFileSystemPolicy,
    pub network: SandboxNetworkPolicy,
    pub environment: SandboxEnvironmentPolicy,
    #[serde(default, skip_serializing_if = "SandboxPlatformPolicy::is_empty")]
    pub platform: SandboxPlatformPolicy,
    pub process: SandboxProcessPolicy,
    pub resources: SandboxResourceLimits,
    pub lifetime: SandboxLifetime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompiledSandboxProfile {
    pub backend: SandboxBackendKind,
    pub enforced: bool,
    pub profile: SandboxProfile,
    pub profile_digest: SandboxProfileDigest,
    pub action_digest: ActionDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityLease {
    pub lease_id: SandboxLeaseId,
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub action_digest: ActionDigest,
    pub profile_digest: SandboxProfileDigest,
    pub actor: Actor,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub one_use: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxOutcome {
    Succeeded,
    Failed,
    Violated,
    Denied,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxViolation {
    pub code: String,
    pub message: String,
    pub resource: Option<String>,
    pub occurred_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxReceipt {
    pub backend: SandboxBackendKind,
    pub enforced: bool,
    pub profile_digest: SandboxProfileDigest,
    pub action_digest: ActionDigest,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub outcome: SandboxOutcome,
    pub exit_code: Option<u32>,
    #[serde(default)]
    pub violations: Vec<SandboxViolation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_reject_ambiguous_text() {
        assert!(ActionDigest::parse("0".repeat(64)).is_ok());
        assert_eq!(
            ActionDigest::parse("A".repeat(64)),
            Err(DigestError::InvalidSha256)
        );
        assert_eq!(
            SandboxProfileDigest::parse("0".repeat(63)),
            Err(DigestError::InvalidSha256)
        );
    }

    #[test]
    fn legacy_proxy_policy_defaults_to_no_unix_socket_authority() {
        let policy: SandboxNetworkPolicy = serde_json::from_value(serde_json::json!({
            "mode": "proxy_only",
            "proxy_url": "http://127.0.0.1:43128",
            "allowed_hosts": ["api.openai.com"]
        }))
        .unwrap();
        assert_eq!(
            policy,
            SandboxNetworkPolicy::ProxyOnly {
                proxy_url: "http://127.0.0.1:43128".into(),
                allowed_hosts: vec!["api.openai.com".into()],
                allowed_unix_sockets: Vec::new(),
            }
        );
    }
}
