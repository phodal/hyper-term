use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hyper_term_core::{
    SandboxCompileRequest, SandboxError, SandboxLaunchPlan, SandboxLauncher,
    canonicalize_sandbox_profile, canonicalize_terminal_command, sandbox_profile_digest,
    terminal_action_digest,
};
use hyper_term_protocol::{
    CompiledSandboxProfile, SandboxBackendKind, SandboxEnforcement, SandboxLifetime,
    SandboxNetworkPolicy, SandboxPathAccess, SandboxPathRule, SandboxProfile, TerminalCommand,
};

pub const MACOS_SEATBELT_EXECUTABLE: &str = "/usr/bin/sandbox-exec";

const BASE_POLICY: &str = include_str!("seatbelt_base_policy.sbpl");
const NETWORK_SUPPORT_POLICY: &str = include_str!("seatbelt_network_policy.sbpl");

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeatbeltPolicyArtifact {
    pub policy: String,
    pub definitions: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeatbeltCompilation {
    pub launch_plan: SandboxLaunchPlan,
    pub artifact: SeatbeltPolicyArtifact,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacOsSeatbeltLauncher;

impl MacOsSeatbeltLauncher {
    pub fn compile_inspectable(
        &self,
        request: &SandboxCompileRequest,
    ) -> Result<SeatbeltCompilation, SandboxError> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            return Err(backend_error(
                "macOS Seatbelt is unavailable on this platform",
            ));
        }

        #[cfg(target_os = "macos")]
        self.compile_macos(request)
    }

    #[cfg(target_os = "macos")]
    fn compile_macos(
        &self,
        request: &SandboxCompileRequest,
    ) -> Result<SeatbeltCompilation, SandboxError> {
        if request.operation_revision == 0 {
            return Err(backend_error("operation revision must be non-zero"));
        }
        validate_supported_contract(&request.profile)?;
        ensure_seatbelt_executable()?;

        let command = resolve_command_for_seatbelt(&request.command)?;
        let mut profile = resolve_profile_for_seatbelt(&request.profile)?;
        add_platform_exec_variants(&command, &mut profile)?;
        let artifact = compile_policy(&profile, &command)?;
        let profile_digest = sandbox_profile_digest(&profile)?;
        let action_digest = terminal_action_digest(&command)?;
        let environment = merged_environment(&profile, &command)?;

        let mut args = vec!["-p".to_string(), artifact.policy.clone()];
        args.extend(
            artifact
                .definitions
                .iter()
                .map(|(key, value)| format!("-D{key}={}", value.to_string_lossy())),
        );
        args.push("--".to_string());
        args.push(command.program.clone());
        args.extend(command.args.clone());

        let launch_plan = SandboxLaunchPlan {
            command: TerminalCommand {
                program: MACOS_SEATBELT_EXECUTABLE.to_string(),
                args,
                cwd: command.cwd.clone(),
                env: environment,
            },
            compiled: CompiledSandboxProfile {
                backend: SandboxBackendKind::MacOsSeatbelt,
                enforced: true,
                profile,
                profile_digest,
                action_digest,
            },
            clear_environment: true,
        };
        Ok(SeatbeltCompilation {
            launch_plan,
            artifact,
        })
    }
}

impl SandboxLauncher for MacOsSeatbeltLauncher {
    fn compile(&self, request: &SandboxCompileRequest) -> Result<SandboxLaunchPlan, SandboxError> {
        self.compile_inspectable(request)
            .map(|compilation| compilation.launch_plan)
    }
}

fn validate_supported_contract(profile: &SandboxProfile) -> Result<(), SandboxError> {
    if profile.enforcement != SandboxEnforcement::Native {
        return Err(backend_error(
            "Seatbelt implements native operation isolation, not isolated tasks",
        ));
    }
    validate_network_policy(&profile.network)?;
    if !matches!(
        profile.lifetime,
        SandboxLifetime::OneOperation | SandboxLifetime::OneTask
    ) {
        return Err(backend_error(
            "Seatbelt does not support the requested sandbox lifetime",
        ));
    }
    if profile.resources.wall_time_ms.is_some()
        || profile.resources.max_processes.is_some()
        || profile.resources.max_output_bytes.is_some()
    {
        return Err(backend_error(
            "Seatbelt cannot enforce the requested resource limits",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_seatbelt_executable() -> Result<(), SandboxError> {
    let metadata = fs::metadata(MACOS_SEATBELT_EXECUTABLE)
        .map_err(|error| backend_error(format!("Seatbelt executable is unavailable: {error}")))?;
    if !metadata.is_file() {
        return Err(backend_error(
            "the pinned Seatbelt executable is not a regular file",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn resolve_profile_for_seatbelt(profile: &SandboxProfile) -> Result<SandboxProfile, SandboxError> {
    let mut profile = canonicalize_sandbox_profile(profile)?;
    let mut rules = BTreeMap::<PathBuf, SandboxPathAccess>::new();
    for rule in &profile.filesystem.rules {
        let path = resolve_path_for_seatbelt(&rule.path)?;
        rules
            .entry(path)
            .and_modify(|current| *current = stricter_access(*current, rule.access))
            .or_insert(rule.access);
    }
    profile.filesystem.rules = rules
        .into_iter()
        .map(|(path, access)| SandboxPathRule { path, access })
        .collect();

    let mut executables = profile
        .process
        .allowed_executables
        .iter()
        .map(|path| resolve_existing_executable(path))
        .collect::<Result<Vec<_>, _>>()?;
    executables.sort();
    executables.dedup();
    profile.process.allowed_executables = executables;
    if let SandboxNetworkPolicy::ProxyOnly {
        allowed_unix_sockets,
        ..
    } = &mut profile.network
    {
        *allowed_unix_sockets = allowed_unix_sockets
            .iter()
            .map(|path| resolve_path_for_seatbelt(path))
            .collect::<Result<Vec<_>, _>>()?;
        allowed_unix_sockets.sort();
        allowed_unix_sockets.dedup();
    }
    Ok(profile)
}

#[cfg(target_os = "macos")]
fn resolve_command_for_seatbelt(
    command: &TerminalCommand,
) -> Result<TerminalCommand, SandboxError> {
    let mut command = canonicalize_terminal_command(command)?;
    command.program = resolve_existing_executable(Path::new(&command.program))?
        .into_os_string()
        .into_string()
        .map_err(|_| backend_error("resolved executable is not valid UTF-8"))?;
    command.cwd = Some(
        command
            .cwd
            .as_deref()
            .ok_or_else(|| {
                backend_error("sandboxed commands require an explicit working directory")
            })
            .and_then(|cwd| {
                let resolved = fs::canonicalize(cwd).map_err(|error| {
                    backend_error(format!(
                        "sandbox working directory {} is unavailable: {error}",
                        cwd.display()
                    ))
                })?;
                if !resolved.is_dir() {
                    return Err(backend_error(format!(
                        "sandbox working directory {} is not a directory",
                        resolved.display()
                    )));
                }
                Ok(resolved)
            })?,
    );
    Ok(command)
}

#[cfg(target_os = "macos")]
fn resolve_existing_executable(path: &Path) -> Result<PathBuf, SandboxError> {
    let resolved = fs::canonicalize(path).map_err(|error| {
        backend_error(format!(
            "sandbox executable {} is unavailable: {error}",
            path.display()
        ))
    })?;
    let metadata = fs::metadata(&resolved).map_err(|error| {
        backend_error(format!(
            "cannot inspect sandbox executable {}: {error}",
            resolved.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(backend_error(format!(
            "sandbox executable {} is not a regular file",
            resolved.display()
        )));
    }
    Ok(resolved)
}

#[cfg(target_os = "macos")]
fn add_platform_exec_variants(
    command: &TerminalCommand,
    profile: &mut SandboxProfile,
) -> Result<(), SandboxError> {
    // Apple's `/bin/sh` launcher selects `/bin/bash` as its compatibility
    // implementation after Seatbelt is active. Bind that OS-required second
    // executable into the compiled profile rather than broadening process-exec.
    if command.program == "/bin/sh" {
        profile
            .process
            .allowed_executables
            .push(resolve_existing_executable(Path::new("/bin/bash"))?);
        profile.process.allowed_executables.sort();
        profile.process.allowed_executables.dedup();
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn resolve_path_for_seatbelt(path: &Path) -> Result<PathBuf, SandboxError> {
    if let Ok(resolved) = fs::canonicalize(path) {
        return Ok(resolved);
    }

    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        let name = ancestor.file_name().ok_or_else(|| {
            backend_error(format!(
                "sandbox path {} has no existing ancestor",
                path.display()
            ))
        })?;
        suffix.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            backend_error(format!(
                "sandbox path {} has no existing ancestor",
                path.display()
            ))
        })?;
        if let Ok(mut resolved) = fs::canonicalize(ancestor) {
            for component in suffix.iter().rev() {
                resolved.push(component);
            }
            return Ok(resolved);
        }
    }
}

#[cfg(target_os = "macos")]
fn compile_policy(
    profile: &SandboxProfile,
    command: &TerminalCommand,
) -> Result<SeatbeltPolicyArtifact, SandboxError> {
    let mut definitions = BTreeMap::new();
    let mut policy = String::from(BASE_POLICY);
    policy.push_str("\n; operation-bound executable policy\n");

    let mut executables = profile.process.allowed_executables.clone();
    executables.push(PathBuf::from(&command.program));
    executables.sort();
    executables.dedup();
    if profile.process.allow_any_executable {
        policy.push_str("(allow process-exec)\n");
    } else {
        for (index, executable) in executables.into_iter().enumerate() {
            let key = format!("EXECUTABLE_{index}");
            definitions.insert(key.clone(), executable);
            policy.push_str(&format!(
                "(allow process-exec (literal (param \"{key}\")))\n"
            ));
        }
    }
    if profile.process.allow_child_processes {
        policy.push_str("(allow process-fork)\n");
    }

    if let Some(port) = proxy_loopback_port(&profile.network)? {
        policy.push_str("\n; exact Rust-owned proxy endpoint\n");
        policy.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
        policy.push_str(NETWORK_SUPPORT_POLICY);
    }
    if let SandboxNetworkPolicy::ProxyOnly {
        allowed_unix_sockets,
        ..
    } = &profile.network
    {
        if !allowed_unix_sockets.is_empty() {
            policy.push_str("(allow system-socket (socket-domain AF_UNIX))\n");
        }
        for (index, socket) in allowed_unix_sockets.iter().enumerate() {
            let key = format!("UNIX_SOCKET_{index}");
            definitions.insert(key.clone(), socket.clone());
            policy.push_str(&format!(
                "(allow network-outbound (remote unix-socket (subpath (param \"{key}\"))))\n"
            ));
        }
    }

    policy.push_str("\n; canonical filesystem policy\n");
    for (index, rule) in profile.filesystem.rules.iter().enumerate() {
        let key = format!("PATH_{index}");
        definitions.insert(key.clone(), rule.path.clone());
        policy.push_str(&format!(
            "(allow file-read-metadata file-test-existence (path-ancestors (param \"{key}\")))\n"
        ));
        let filter =
            format!("(require-any (literal (param \"{key}\")) (subpath (param \"{key}\")))");
        match rule.access {
            SandboxPathAccess::Read => {
                policy.push_str(&format!("(allow file-read* {filter})\n"));
                policy.push_str(&format!("(deny file-write* {filter})\n"));
            }
            SandboxPathAccess::Write => {
                policy.push_str(&format!("(allow file-read* file-write* {filter})\n"));
            }
            SandboxPathAccess::Deny => {
                policy.push_str(&format!("(deny file-read* {filter})\n"));
                policy.push_str(&format!("(deny file-write* {filter})\n"));
            }
        }
    }

    if policy.contains("\0") {
        return Err(backend_error("compiled Seatbelt policy contains NUL"));
    }
    Ok(SeatbeltPolicyArtifact {
        policy,
        definitions,
    })
}

fn validate_network_policy(network: &SandboxNetworkPolicy) -> Result<(), SandboxError> {
    if let SandboxNetworkPolicy::ProxyOnly { allowed_hosts, .. } = network
        && allowed_hosts.is_empty()
    {
        return Err(backend_error(
            "Seatbelt proxy-only networking requires an explicit host allowlist",
        ));
    }
    proxy_loopback_port(network).map(|_| ())
}

fn proxy_loopback_port(network: &SandboxNetworkPolicy) -> Result<Option<u16>, SandboxError> {
    let SandboxNetworkPolicy::ProxyOnly { proxy_url, .. } = network else {
        return Ok(None);
    };
    let Some(authority) = proxy_url.strip_prefix("http://") else {
        return Err(backend_error(
            "Seatbelt proxy-only networking requires an HTTP loopback endpoint",
        ));
    };
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@'])
        || authority.chars().any(char::is_whitespace)
    {
        return Err(backend_error(
            "Seatbelt proxy endpoint must not contain credentials, paths, queries, or fragments",
        ));
    }
    let Some((host, port)) = authority.split_once(':') else {
        return Err(backend_error(
            "Seatbelt proxy endpoint requires an explicit port",
        ));
    };
    if host != "127.0.0.1" || port.contains(':') {
        return Err(backend_error(
            "Seatbelt proxy endpoint must use the IPv4 loopback address",
        ));
    }
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| backend_error("Seatbelt proxy endpoint has an invalid port"))?;
    Ok(Some(port))
}

#[cfg(target_os = "macos")]
fn merged_environment(
    profile: &SandboxProfile,
    command: &TerminalCommand,
) -> Result<BTreeMap<String, String>, SandboxError> {
    let mut environment = profile.environment.variables.clone();
    for (key, value) in &command.env {
        if let Some(profile_value) = environment.get(key)
            && profile_value != value
        {
            return Err(backend_error(format!(
                "command environment conflicts with sandbox-owned variable {key}"
            )));
        }
        environment.insert(key.clone(), value.clone());
    }
    Ok(environment)
}

#[cfg(target_os = "macos")]
fn stricter_access(left: SandboxPathAccess, right: SandboxPathAccess) -> SandboxPathAccess {
    use SandboxPathAccess as Access;
    match (left, right) {
        (Access::Deny, _) | (_, Access::Deny) => Access::Deny,
        (Access::Write, _) | (_, Access::Write) => Access::Write,
        _ => Access::Read,
    }
}

fn backend_error(message: impl Into<String>) -> SandboxError {
    SandboxError::Backend(message.into())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Output};

    use hyper_term_protocol::{
        Actor, OperationId, SandboxEnvironmentPolicy, SandboxFileSystemPolicy,
        SandboxProcessPolicy, SandboxResourceLimits,
    };
    use tempfile::TempDir;

    use super::*;

    fn runtime_read_rules() -> Vec<SandboxPathRule> {
        ["/System", "/usr", "/bin", "/sbin", "/Library"]
            .into_iter()
            .map(|path| SandboxPathRule {
                path: PathBuf::from(path),
                access: SandboxPathAccess::Read,
            })
            .collect()
    }

    fn profile(workspace: &Path, scratch: &Path) -> SandboxProfile {
        let mut rules = runtime_read_rules();
        rules.extend([
            SandboxPathRule {
                path: workspace.to_path_buf(),
                access: SandboxPathAccess::Write,
            },
            SandboxPathRule {
                path: workspace.join(".git"),
                access: SandboxPathAccess::Read,
            },
            SandboxPathRule {
                path: scratch.to_path_buf(),
                access: SandboxPathAccess::Write,
            },
        ]);
        SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy { rules },
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy {
                clear_inherited: true,
                variables: BTreeMap::from([
                    ("HOME".into(), scratch.to_string_lossy().into_owned()),
                    ("TMPDIR".into(), scratch.to_string_lossy().into_owned()),
                    ("LANG".into(), "C.UTF-8".into()),
                    ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
                    ("TERM".into(), "xterm-256color".into()),
                ]),
            },
            process: SandboxProcessPolicy {
                allow_child_processes: true,
                allow_any_executable: false,
                allowed_executables: vec![PathBuf::from("/usr/bin/nc")],
            },
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneOperation,
        }
    }

    fn request(
        profile: SandboxProfile,
        script: &str,
        arguments: Vec<String>,
    ) -> SandboxCompileRequest {
        let cwd = profile
            .filesystem
            .rules
            .iter()
            .find(|rule| rule.access == SandboxPathAccess::Write)
            .unwrap()
            .path
            .clone();
        let mut args = vec!["-c".into(), script.into(), "hyper-term-sandbox".into()];
        args.extend(arguments);
        SandboxCompileRequest {
            operation_id: OperationId::new(),
            operation_revision: 4,
            actor: Actor::Agent {
                adapter: "test".into(),
            },
            command: TerminalCommand {
                program: "/bin/sh".into(),
                args,
                cwd: Some(cwd),
                env: BTreeMap::new(),
            },
            profile,
        }
    }

    fn run(request: SandboxCompileRequest) -> Output {
        let plan = MacOsSeatbeltLauncher.compile(&request).unwrap();
        let mut command = Command::new(&plan.command.program);
        command.args(&plan.command.args).env_clear();
        command.envs(&plan.command.env);
        if let Some(cwd) = &plan.command.cwd {
            command.current_dir(cwd);
        }
        command.output().unwrap()
    }

    struct Fixture {
        _root: TempDir,
        workspace: PathBuf,
        scratch: PathBuf,
        outside: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            let scratch = root.path().join("scratch");
            let outside = root.path().join("outside.txt");
            fs::create_dir_all(workspace.join(".git")).unwrap();
            fs::create_dir_all(&scratch).unwrap();
            fs::write(workspace.join(".git/config"), "trusted metadata\n").unwrap();
            fs::write(&outside, "host secret\n").unwrap();
            Self {
                _root: root,
                workspace,
                scratch,
                outside,
            }
        }

        fn profile(&self) -> SandboxProfile {
            profile(&self.workspace, &self.scratch)
        }
    }

    #[test]
    fn policy_uses_definitions_instead_of_interpolating_paths() {
        let fixture = Fixture::new();
        let request = request(fixture.profile(), "printf ok", Vec::new());
        let compiled = MacOsSeatbeltLauncher.compile_inspectable(&request).unwrap();
        assert!(compiled.launch_plan.compiled.enforced);
        assert_eq!(
            compiled.launch_plan.compiled.backend,
            SandboxBackendKind::MacOsSeatbelt
        );
        assert!(compiled.artifact.policy.contains("(deny default)"));
        assert!(compiled.artifact.policy.contains("(param \"PATH_"));
        assert!(compiled.artifact.policy.contains("path-ancestors (param"));
        assert!(
            !compiled
                .artifact
                .policy
                .contains(&fixture.workspace.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn unsupported_contracts_fail_closed() {
        let fixture = Fixture::new();
        let base = fixture.profile();

        let mut isolated = base.clone();
        isolated.enforcement = SandboxEnforcement::IsolatedTask;
        assert!(
            MacOsSeatbeltLauncher
                .compile(&request(isolated, "true", Vec::new()))
                .is_err()
        );

        let mut limited = base;
        limited.resources.wall_time_ms = Some(1_000);
        assert!(
            MacOsSeatbeltLauncher
                .compile(&request(limited, "true", Vec::new()))
                .is_err()
        );
    }

    #[test]
    fn proxy_only_policy_allows_exactly_one_loopback_port() {
        let fixture = Fixture::new();
        let allowed = TcpListener::bind("127.0.0.1:0").unwrap();
        let allowed_port = allowed.local_addr().unwrap().port();
        let denied = TcpListener::bind("127.0.0.1:0").unwrap();
        let denied_port = denied.local_addr().unwrap().port();
        let allowed_socket = fixture.scratch.join("broker.sock");
        let denied_socket = fixture.scratch.join("other.sock");
        let allowed_socket_listener = UnixListener::bind(&allowed_socket).unwrap();
        let _denied_socket_listener = UnixListener::bind(&denied_socket).unwrap();
        let resolved_allowed_socket = fs::canonicalize(&allowed_socket).unwrap();
        let resolved_denied_socket = fs::canonicalize(&denied_socket).unwrap();
        let mut proxy_profile = fixture.profile();
        proxy_profile.network = SandboxNetworkPolicy::ProxyOnly {
            proxy_url: format!("http://127.0.0.1:{allowed_port}"),
            allowed_hosts: vec!["api.openai.com".into()],
            allowed_unix_sockets: vec![allowed_socket.clone()],
        };

        let compiled = MacOsSeatbeltLauncher
            .compile_inspectable(&request(proxy_profile.clone(), "true", Vec::new()))
            .unwrap();
        assert!(compiled.artifact.policy.contains(&format!(
            "(allow network-outbound (remote ip \"localhost:{allowed_port}\"))"
        )));
        assert!(!compiled.artifact.policy.contains(&format!(
            "(allow network-outbound (remote ip \"localhost:{denied_port}\"))"
        )));
        assert!(!compiled.artifact.policy.contains("(remote ip \"*:53\")"));
        assert!(!compiled.artifact.policy.contains("(allow network-inbound"));
        assert!(
            !compiled
                .artifact
                .policy
                .contains("(allow network-outbound)")
        );
        assert!(compiled.artifact.policy.contains(
            "(allow network-outbound (remote unix-socket (subpath (param \"UNIX_SOCKET_0\"))))"
        ));
        assert!(
            compiled
                .artifact
                .definitions
                .values()
                .any(|path| path == &resolved_allowed_socket)
        );
        assert!(
            !compiled
                .artifact
                .definitions
                .values()
                .any(|path| path == &resolved_denied_socket)
        );
        let SandboxNetworkPolicy::ProxyOnly {
            proxy_url,
            allowed_hosts,
            allowed_unix_sockets,
        } = &compiled.launch_plan.compiled.profile.network
        else {
            panic!("expected proxy-only profile");
        };
        assert_eq!(proxy_url, &format!("http://127.0.0.1:{allowed_port}"));
        assert_eq!(allowed_hosts, &["api.openai.com"]);
        assert_eq!(allowed_unix_sockets, &[resolved_allowed_socket]);

        let output = run(request(
            proxy_profile.clone(),
            "/usr/bin/nc -z 127.0.0.1 \"$1\"",
            vec![allowed_port.to_string()],
        ));
        assert!(
            output.status.success(),
            "allowed proxy port failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let output = run(request(
            proxy_profile.clone(),
            "/usr/bin/nc -z 127.0.0.1 \"$1\"",
            vec![denied_port.to_string()],
        ));
        assert!(!output.status.success());

        allowed_socket_listener.set_nonblocking(true).unwrap();
        let accepted = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match allowed_socket_listener.accept() {
                    Ok((stream, _)) => {
                        drop(stream);
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            panic!("sandboxed client did not reach the allowed broker socket");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("broker socket accept failed: {error}"),
                }
            }
        });
        let output = run(request(
            proxy_profile.clone(),
            "printf connected | /usr/bin/nc -U \"$1\"",
            vec![allowed_socket.to_string_lossy().into_owned()],
        ));
        accepted.join().unwrap();
        assert!(
            output.status.success(),
            "allowed broker socket failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = run(request(
            proxy_profile,
            "/usr/bin/nc -zU \"$1\"",
            vec![denied_socket.to_string_lossy().into_owned()],
        ));
        assert!(!output.status.success());
    }

    #[test]
    fn proxy_only_policy_rejects_ambiguous_or_credentialed_endpoints() {
        let fixture = Fixture::new();
        for proxy_url in [
            "https://127.0.0.1:43128",
            "http://localhost:43128",
            "http://user:secret@127.0.0.1:43128",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://127.0.0.1:43128/path",
            "http://127.0.0.1:43128?query",
        ] {
            let mut invalid = fixture.profile();
            invalid.network = SandboxNetworkPolicy::ProxyOnly {
                proxy_url: proxy_url.into(),
                allowed_hosts: vec!["api.openai.com".into()],
                allowed_unix_sockets: Vec::new(),
            };
            assert!(
                MacOsSeatbeltLauncher
                    .compile(&request(invalid, "true", Vec::new()))
                    .is_err(),
                "accepted invalid proxy URL {proxy_url}"
            );
        }

        let mut empty_allowlist = fixture.profile();
        empty_allowlist.network = SandboxNetworkPolicy::ProxyOnly {
            proxy_url: "http://127.0.0.1:43128".into(),
            allowed_hosts: Vec::new(),
            allowed_unix_sockets: Vec::new(),
        };
        assert!(
            MacOsSeatbeltLauncher
                .compile(&request(empty_allowlist, "true", Vec::new()))
                .is_err()
        );
    }

    #[test]
    fn seatbelt_allows_workspace_and_scratch_but_denies_host_and_metadata() {
        let fixture = Fixture::new();

        let workspace_file = fixture.workspace.join("allowed.txt");
        let output = run(request(
            fixture.profile(),
            "printf allowed > \"$1\"",
            vec![workspace_file.to_string_lossy().into_owned()],
        ));
        assert!(
            output.status.success(),
            "status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(&workspace_file).unwrap(), "allowed");

        let scratch_file = fixture.scratch.join("allowed.txt");
        let output = run(request(
            fixture.profile(),
            "printf scratch > \"$1\"",
            vec![scratch_file.to_string_lossy().into_owned()],
        ));
        assert!(
            output.status.success(),
            "status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let output = run(request(
            fixture.profile(),
            "printf denied > \"$1\"",
            vec![fixture.outside.to_string_lossy().into_owned()],
        ));
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(&fixture.outside).unwrap(),
            "host secret\n"
        );

        let output = run(request(
            fixture.profile(),
            "IFS= read -r value < \"$1\" && printf '%s' \"$value\"",
            vec![fixture.outside.to_string_lossy().into_owned()],
        ));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());

        let git_config = fixture.workspace.join(".git/config");
        let output = run(request(
            fixture.profile(),
            "IFS= read -r value < \"$1\" && printf '%s' \"$value\"",
            vec![git_config.to_string_lossy().into_owned()],
        ));
        assert!(output.status.success());
        assert_eq!(output.stdout, b"trusted metadata");
        let output = run(request(
            fixture.profile(),
            "printf denied > \"$1\"",
            vec![git_config.to_string_lossy().into_owned()],
        ));
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(git_config).unwrap(),
            "trusted metadata\n"
        );
    }

    #[test]
    fn seatbelt_denies_symlink_escape_and_child_network_access() {
        let fixture = Fixture::new();
        let escape = fixture.workspace.join("escape.txt");
        symlink(&fixture.outside, &escape).unwrap();
        let output = run(request(
            fixture.profile(),
            "printf escaped > \"$1\"",
            vec![escape.to_string_lossy().into_owned()],
        ));
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(&fixture.outside).unwrap(),
            "host secret\n"
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let output = run(request(
            fixture.profile(),
            "/usr/bin/nc -z 127.0.0.1 \"$1\"",
            vec![port.to_string()],
        ));
        assert!(!output.status.success());
        drop(listener);
    }

    #[test]
    fn inherited_environment_is_cleared() {
        let fixture = Fixture::new();
        let plan = MacOsSeatbeltLauncher
            .compile(&request(fixture.profile(), "true", Vec::new()))
            .unwrap();
        assert!(plan.clear_environment);
        assert_eq!(
            plan.command.env.get("LANG").map(String::as_str),
            Some("C.UTF-8")
        );
        assert!(!plan.command.env.contains_key("USER"));
        assert!(!plan.command.env.contains_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn child_process_execution_is_allowlisted() {
        let fixture = Fixture::new();
        let target = fixture.workspace.join("not-created.txt");
        let output = run(request(
            fixture.profile(),
            "/usr/bin/touch \"$1\"",
            vec![target.to_string_lossy().into_owned()],
        ));
        assert!(!output.status.success());
        assert!(!target.exists());
    }

    #[test]
    fn command_environment_cannot_override_authority_environment() {
        let fixture = Fixture::new();
        let mut request = request(fixture.profile(), "true", Vec::new());
        request
            .command
            .env
            .insert("HOME".into(), "/tmp/escape".into());
        assert!(MacOsSeatbeltLauncher.compile(&request).is_err());
    }

    #[test]
    fn test_fixture_can_write_before_entering_the_sandbox() {
        let fixture = Fixture::new();
        let path = fixture.workspace.join("host.txt");
        let mut file = fs::File::create(path).unwrap();
        file.write_all(b"host").unwrap();
    }
}
