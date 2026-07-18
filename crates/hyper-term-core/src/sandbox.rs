use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use hyper_term_protocol::{
    ActionDigest, Actor, CapabilityLease, CompiledSandboxProfile, OperationId, SandboxBackendKind,
    SandboxLeaseId, SandboxPathAccess, SandboxPathRule, SandboxProfile, SandboxProfileDigest,
    TerminalCommand,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxCompileRequest {
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub actor: Actor,
    pub command: TerminalCommand,
    pub profile: SandboxProfile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxLaunchPlan {
    pub command: TerminalCommand,
    pub compiled: CompiledSandboxProfile,
    pub clear_environment: bool,
}

pub trait SandboxLauncher: Send + Sync {
    fn compile(&self, request: &SandboxCompileRequest) -> Result<SandboxLaunchPlan, SandboxError>;
}

/// An explicitly unenforced backend for reducer and daemon tests.
///
/// The compiled contract records `enforced = false`, so callers cannot mistake
/// this launcher for an operating-system security boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestOnlyUnenforcedSandboxLauncher;

impl SandboxLauncher for TestOnlyUnenforcedSandboxLauncher {
    fn compile(&self, request: &SandboxCompileRequest) -> Result<SandboxLaunchPlan, SandboxError> {
        let profile = canonicalize_sandbox_profile(&request.profile)?;
        let command = canonicalize_terminal_command(&request.command)?;
        let profile_digest = sandbox_profile_digest(&profile)?;
        let action_digest = terminal_action_digest(&command)?;
        Ok(SandboxLaunchPlan {
            command,
            compiled: CompiledSandboxProfile {
                backend: SandboxBackendKind::TestOnlyUnenforced,
                enforced: false,
                profile,
                profile_digest,
                action_digest,
            },
            clear_environment: true,
        })
    }
}

pub fn canonicalize_sandbox_profile(
    profile: &SandboxProfile,
) -> Result<SandboxProfile, SandboxError> {
    if !profile.environment.clear_inherited {
        return Err(SandboxError::InheritedEnvironmentNotCleared);
    }
    for (name, value) in &profile.environment.variables {
        if name.is_empty() || name.contains('=') || name.contains('\0') {
            return Err(SandboxError::InvalidEnvironmentName(name.clone()));
        }
        if value.contains('\0') {
            return Err(SandboxError::InvalidEnvironmentValue(name.clone()));
        }
    }
    if profile.resources.wall_time_ms == Some(0)
        || profile.resources.max_processes == Some(0)
        || profile.resources.max_output_bytes == Some(0)
    {
        return Err(SandboxError::ZeroResourceLimit);
    }
    if profile.process.allow_any_executable && !profile.process.allow_child_processes {
        return Err(SandboxError::AnyExecutableRequiresChildProcesses);
    }

    let mut canonical = profile.clone();
    let mut rules = BTreeMap::<PathBuf, SandboxPathAccess>::new();
    for rule in &profile.filesystem.rules {
        let path = normalize_absolute_path(&rule.path)?;
        rules
            .entry(path)
            .and_modify(|existing| *existing = stricter_access(*existing, rule.access))
            .or_insert(rule.access);
    }
    canonical.filesystem.rules = rules
        .into_iter()
        .map(|(path, access)| SandboxPathRule { path, access })
        .collect();

    let mut executables = profile
        .process
        .allowed_executables
        .iter()
        .map(|path| normalize_absolute_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    executables.sort();
    executables.dedup();
    canonical.process.allowed_executables = executables;

    if let hyper_term_protocol::SandboxNetworkPolicy::ProxyOnly {
        proxy_url,
        allowed_hosts,
    } = &mut canonical.network
    {
        if proxy_url.trim().is_empty() {
            return Err(SandboxError::EmptyProxyUrl);
        }
        for host in allowed_hosts.iter_mut() {
            *host = host.trim().to_ascii_lowercase();
            if host.is_empty() || host.contains('/') || host.contains('\0') {
                return Err(SandboxError::InvalidAllowedHost(host.clone()));
            }
        }
        allowed_hosts.sort();
        allowed_hosts.dedup();
    }

    Ok(canonical)
}

pub fn canonicalize_terminal_command(
    command: &TerminalCommand,
) -> Result<TerminalCommand, SandboxError> {
    let mut canonical = command.clone();
    canonical.program = normalize_absolute_path(Path::new(&command.program))?
        .into_os_string()
        .into_string()
        .map_err(|_| SandboxError::NonUtf8Executable)?;
    canonical.cwd = command
        .cwd
        .as_deref()
        .map(normalize_absolute_path)
        .transpose()?;
    for (name, value) in &command.env {
        if name.is_empty() || name.contains('=') || name.contains('\0') {
            return Err(SandboxError::InvalidEnvironmentName(name.clone()));
        }
        if value.contains('\0') {
            return Err(SandboxError::InvalidEnvironmentValue(name.clone()));
        }
    }
    if command.args.iter().any(|argument| argument.contains('\0')) {
        return Err(SandboxError::NulArgument);
    }
    Ok(canonical)
}

pub fn sandbox_profile_digest(
    profile: &SandboxProfile,
) -> Result<SandboxProfileDigest, SandboxError> {
    let canonical = canonicalize_sandbox_profile(profile)?;
    sha256_json(&canonical).and_then(|digest| {
        SandboxProfileDigest::parse(digest).map_err(|error| SandboxError::Digest(error.to_string()))
    })
}

pub fn terminal_action_digest(command: &TerminalCommand) -> Result<ActionDigest, SandboxError> {
    let canonical = canonicalize_terminal_command(command)?;
    sha256_json(&canonical).and_then(|digest| {
        ActionDigest::parse(digest).map_err(|error| SandboxError::Digest(error.to_string()))
    })
}

fn sha256_json(value: &impl Serialize) -> Result<String, SandboxError> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf, SandboxError> {
    if !path.is_absolute() {
        return Err(SandboxError::RelativePath(path.to_path_buf()));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(SandboxError::ParentTraversal(path.to_path_buf()));
            }
        }
    }
    Ok(normalized)
}

fn stricter_access(left: SandboxPathAccess, right: SandboxPathAccess) -> SandboxPathAccess {
    use SandboxPathAccess as Access;
    match (left, right) {
        (Access::Deny, _) | (_, Access::Deny) => Access::Deny,
        (Access::Write, _) | (_, Access::Write) => Access::Write,
        _ => Access::Read,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxLeaseExpectation {
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub action_digest: ActionDigest,
    pub profile_digest: SandboxProfileDigest,
    pub actor: Actor,
}

#[derive(Clone, Debug)]
struct LeaseEntry {
    lease: CapabilityLease,
    consumed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct CapabilityLeaseLedger {
    leases: HashMap<SandboxLeaseId, LeaseEntry>,
}

impl CapabilityLeaseLedger {
    pub fn issue(&mut self, lease: CapabilityLease) -> Result<(), SandboxLeaseError> {
        if lease.operation_revision == 0 {
            return Err(SandboxLeaseError::ZeroRevision);
        }
        if lease.expires_at_ms <= lease.issued_at_ms {
            return Err(SandboxLeaseError::InvalidLifetime);
        }
        if !lease.one_use {
            return Err(SandboxLeaseError::ReusableLeaseForbidden);
        }
        if self.leases.contains_key(&lease.lease_id) {
            return Err(SandboxLeaseError::AlreadyExists(lease.lease_id));
        }
        self.leases.insert(
            lease.lease_id,
            LeaseEntry {
                lease,
                consumed_at_ms: None,
            },
        );
        Ok(())
    }

    pub fn consume(
        &mut self,
        lease_id: SandboxLeaseId,
        expected: &SandboxLeaseExpectation,
        now_ms: u64,
    ) -> Result<CapabilityLease, SandboxLeaseError> {
        let entry = self
            .leases
            .get_mut(&lease_id)
            .ok_or(SandboxLeaseError::NotFound(lease_id))?;
        if entry.consumed_at_ms.is_some() {
            return Err(SandboxLeaseError::AlreadyConsumed(lease_id));
        }
        if now_ms < entry.lease.issued_at_ms {
            return Err(SandboxLeaseError::NotYetValid);
        }
        if now_ms >= entry.lease.expires_at_ms {
            return Err(SandboxLeaseError::Expired);
        }
        if entry.lease.operation_id != expected.operation_id {
            return Err(SandboxLeaseError::OperationMismatch);
        }
        if entry.lease.operation_revision != expected.operation_revision {
            return Err(SandboxLeaseError::RevisionMismatch {
                expected: entry.lease.operation_revision,
                actual: expected.operation_revision,
            });
        }
        if entry.lease.action_digest != expected.action_digest {
            return Err(SandboxLeaseError::ActionDigestMismatch);
        }
        if entry.lease.profile_digest != expected.profile_digest {
            return Err(SandboxLeaseError::ProfileDigestMismatch);
        }
        if entry.lease.actor != expected.actor {
            return Err(SandboxLeaseError::ActorMismatch);
        }
        entry.consumed_at_ms = Some(now_ms);
        Ok(entry.lease.clone())
    }

    pub fn is_consumed(&self, lease_id: SandboxLeaseId) -> Option<bool> {
        self.leases
            .get(&lease_id)
            .map(|entry| entry.consumed_at_ms.is_some())
    }
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox paths must be absolute: {0}")]
    RelativePath(PathBuf),
    #[error("sandbox paths may not contain parent traversal: {0}")]
    ParentTraversal(PathBuf),
    #[error("sandbox executable path is not valid UTF-8")]
    NonUtf8Executable,
    #[error("sandbox command arguments may not contain NUL")]
    NulArgument,
    #[error("sandbox environment must clear inherited variables")]
    InheritedEnvironmentNotCleared,
    #[error("invalid sandbox environment variable name {0:?}")]
    InvalidEnvironmentName(String),
    #[error("sandbox environment variable {0:?} contains NUL")]
    InvalidEnvironmentValue(String),
    #[error("sandbox resource limits must be greater than zero")]
    ZeroResourceLimit,
    #[error("allow_any_executable requires allow_child_processes")]
    AnyExecutableRequiresChildProcesses,
    #[error("proxy-only sandbox policy requires a proxy URL")]
    EmptyProxyUrl,
    #[error("invalid proxy allow-list host {0:?}")]
    InvalidAllowedHost(String),
    #[error("failed to serialize sandbox digest input: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid sandbox digest: {0}")]
    Digest(String),
    #[error("sandbox backend rejected the request: {0}")]
    Backend(String),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SandboxLeaseError {
    #[error("sandbox lease operation revision must be non-zero")]
    ZeroRevision,
    #[error("sandbox lease must expire after it is issued")]
    InvalidLifetime,
    #[error("reusable sandbox leases are forbidden")]
    ReusableLeaseForbidden,
    #[error("sandbox lease {0} already exists")]
    AlreadyExists(SandboxLeaseId),
    #[error("sandbox lease {0} does not exist")]
    NotFound(SandboxLeaseId),
    #[error("sandbox lease {0} was already consumed")]
    AlreadyConsumed(SandboxLeaseId),
    #[error("sandbox lease is not yet valid")]
    NotYetValid,
    #[error("sandbox lease expired")]
    Expired,
    #[error("sandbox lease operation does not match")]
    OperationMismatch,
    #[error("sandbox lease revision mismatch: lease binds {expected}, request uses {actual}")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("sandbox lease action digest does not match")]
    ActionDigestMismatch,
    #[error("sandbox lease profile digest does not match")]
    ProfileDigestMismatch,
    #[error("sandbox lease actor does not match")]
    ActorMismatch,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hyper_term_protocol::{
        SandboxEnforcement, SandboxEnvironmentPolicy, SandboxFileSystemPolicy, SandboxLifetime,
        SandboxNetworkPolicy, SandboxProcessPolicy, SandboxResourceLimits,
    };

    use super::*;

    fn profile(rules: Vec<SandboxPathRule>) -> SandboxProfile {
        SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy { rules },
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy::default(),
            process: SandboxProcessPolicy::default(),
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneOperation,
        }
    }

    fn command() -> TerminalCommand {
        TerminalCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf ok".into()],
            cwd: Some(PathBuf::from("/tmp/workspace")),
            env: BTreeMap::from([("TERM".into(), "xterm-256color".into())]),
        }
    }

    #[test]
    fn profile_digest_is_stable_across_rule_order_and_duplicates() {
        let first = profile(vec![
            SandboxPathRule {
                path: "/tmp/workspace".into(),
                access: SandboxPathAccess::Read,
            },
            SandboxPathRule {
                path: "/tmp/workspace".into(),
                access: SandboxPathAccess::Write,
            },
            SandboxPathRule {
                path: "/usr".into(),
                access: SandboxPathAccess::Read,
            },
        ]);
        let second = profile(vec![
            SandboxPathRule {
                path: "/usr".into(),
                access: SandboxPathAccess::Read,
            },
            SandboxPathRule {
                path: "/tmp/workspace".into(),
                access: SandboxPathAccess::Write,
            },
        ]);
        assert_eq!(
            sandbox_profile_digest(&first).unwrap(),
            sandbox_profile_digest(&second).unwrap()
        );
        let canonical = canonicalize_sandbox_profile(&first).unwrap();
        assert_eq!(
            canonical
                .filesystem
                .rules
                .iter()
                .find(|rule| rule.path == Path::new("/tmp/workspace"))
                .unwrap()
                .access,
            SandboxPathAccess::Write
        );
    }

    #[test]
    fn deny_wins_for_the_same_canonical_path() {
        let canonical = canonicalize_sandbox_profile(&profile(vec![
            SandboxPathRule {
                path: "/tmp/workspace".into(),
                access: SandboxPathAccess::Write,
            },
            SandboxPathRule {
                path: "/tmp/workspace/.git".into(),
                access: SandboxPathAccess::Deny,
            },
            SandboxPathRule {
                path: "/tmp/workspace".into(),
                access: SandboxPathAccess::Deny,
            },
        ]))
        .unwrap();
        assert_eq!(canonical.filesystem.rules.len(), 2);
        assert!(
            canonical
                .filesystem
                .rules
                .iter()
                .all(|rule| rule.access == SandboxPathAccess::Deny)
        );
    }

    #[test]
    fn paths_reject_relative_and_parent_traversal() {
        let relative = profile(vec![SandboxPathRule {
            path: "workspace".into(),
            access: SandboxPathAccess::Read,
        }]);
        assert!(matches!(
            canonicalize_sandbox_profile(&relative),
            Err(SandboxError::RelativePath(_))
        ));
        let traversal = profile(vec![SandboxPathRule {
            path: "/tmp/../etc".into(),
            access: SandboxPathAccess::Read,
        }]);
        assert!(matches!(
            canonicalize_sandbox_profile(&traversal),
            Err(SandboxError::ParentTraversal(_))
        ));
    }

    #[test]
    fn any_child_executable_requires_child_process_authority() {
        let mut profile = profile(Vec::new());
        profile.process.allow_any_executable = true;
        assert!(matches!(
            canonicalize_sandbox_profile(&profile),
            Err(SandboxError::AnyExecutableRequiresChildProcesses)
        ));
    }

    #[test]
    fn action_digest_binds_every_command_field() {
        let original = command();
        let original_digest = terminal_action_digest(&original).unwrap();

        let mut changed = original.clone();
        changed.args.push("extra".into());
        assert_ne!(original_digest, terminal_action_digest(&changed).unwrap());

        let mut changed = original.clone();
        changed.cwd = Some("/tmp/other".into());
        assert_ne!(original_digest, terminal_action_digest(&changed).unwrap());

        let mut changed = original;
        changed.env.insert("LANG".into(), "C".into());
        assert_ne!(original_digest, terminal_action_digest(&changed).unwrap());
    }

    fn lease() -> CapabilityLease {
        CapabilityLease {
            lease_id: SandboxLeaseId::new(),
            operation_id: OperationId::new(),
            operation_revision: 4,
            action_digest: terminal_action_digest(&command()).unwrap(),
            profile_digest: sandbox_profile_digest(&profile(Vec::new())).unwrap(),
            actor: Actor::Agent {
                adapter: "test".into(),
            },
            issued_at_ms: 100,
            expires_at_ms: 200,
            one_use: true,
        }
    }

    fn expectation(lease: &CapabilityLease) -> SandboxLeaseExpectation {
        SandboxLeaseExpectation {
            operation_id: lease.operation_id,
            operation_revision: lease.operation_revision,
            action_digest: lease.action_digest.clone(),
            profile_digest: lease.profile_digest.clone(),
            actor: lease.actor.clone(),
        }
    }

    #[test]
    fn lease_is_revision_bound_and_consumed_once() {
        let lease = lease();
        let mut ledger = CapabilityLeaseLedger::default();
        ledger.issue(lease.clone()).unwrap();

        let mut wrong_revision = expectation(&lease);
        wrong_revision.operation_revision += 1;
        assert!(matches!(
            ledger.consume(lease.lease_id, &wrong_revision, 150),
            Err(SandboxLeaseError::RevisionMismatch { .. })
        ));
        assert_eq!(ledger.is_consumed(lease.lease_id), Some(false));

        ledger
            .consume(lease.lease_id, &expectation(&lease), 150)
            .unwrap();
        assert_eq!(ledger.is_consumed(lease.lease_id), Some(true));
        assert_eq!(
            ledger.consume(lease.lease_id, &expectation(&lease), 151),
            Err(SandboxLeaseError::AlreadyConsumed(lease.lease_id))
        );
    }

    #[test]
    fn lease_rejects_action_profile_actor_and_expiry_changes() {
        let variants = ["action", "profile", "actor"];
        for variant in variants {
            let lease = lease();
            let mut ledger = CapabilityLeaseLedger::default();
            ledger.issue(lease.clone()).unwrap();
            let mut expected = expectation(&lease);
            let wanted = match variant {
                "action" => {
                    expected.action_digest = ActionDigest::parse("1".repeat(64)).unwrap();
                    SandboxLeaseError::ActionDigestMismatch
                }
                "profile" => {
                    expected.profile_digest = SandboxProfileDigest::parse("2".repeat(64)).unwrap();
                    SandboxLeaseError::ProfileDigestMismatch
                }
                _ => {
                    expected.actor = Actor::User;
                    SandboxLeaseError::ActorMismatch
                }
            };
            assert_eq!(ledger.consume(lease.lease_id, &expected, 150), Err(wanted));
        }

        let lease = lease();
        let mut ledger = CapabilityLeaseLedger::default();
        ledger.issue(lease.clone()).unwrap();
        assert_eq!(
            ledger.consume(lease.lease_id, &expectation(&lease), 200),
            Err(SandboxLeaseError::Expired)
        );
    }

    #[test]
    fn fake_launcher_is_visibly_unenforced() {
        let request = SandboxCompileRequest {
            operation_id: OperationId::new(),
            operation_revision: 4,
            actor: Actor::Agent {
                adapter: "test".into(),
            },
            command: command(),
            profile: profile(Vec::new()),
        };
        let plan = TestOnlyUnenforcedSandboxLauncher.compile(&request).unwrap();
        assert!(!plan.compiled.enforced);
        assert_eq!(
            plan.compiled.backend,
            SandboxBackendKind::TestOnlyUnenforced
        );
        assert!(plan.clear_environment);
    }
}
