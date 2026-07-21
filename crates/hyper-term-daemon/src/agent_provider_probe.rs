use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

const PROVIDER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROVIDER_PROBE_MAX_STDOUT_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub struct AcpAgentProviderConfig {
    pub provider_id: String,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<String, OsString>,
    pub implementation_version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderReadiness {
    Authenticated,
    Available,
    LoginRequired,
    ProviderMissing,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderContainment {
    NativeSeatbelt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AgentProviderStatus {
    pub id: String,
    pub protocol: String,
    pub readiness: AgentProviderReadiness,
    pub containment: AgentProviderContainment,
}

impl AgentProviderStatus {
    fn new(id: &str, protocol: &str, readiness: AgentProviderReadiness) -> Self {
        Self {
            id: id.into(),
            protocol: protocol.into(),
            readiness,
            containment: AgentProviderContainment::NativeSeatbelt,
        }
    }

    pub fn usable(&self) -> bool {
        matches!(
            self.readiness,
            AgentProviderReadiness::Authenticated | AgentProviderReadiness::Available
        )
    }
}

pub(crate) struct AgentProviderProbeConfig<'a> {
    pub provider_home: &'a Path,
    pub codex_executable: Option<&'a Path>,
    pub codex_auth_file: Option<&'a Path>,
    pub acp_providers: &'a [AcpAgentProviderConfig],
}

#[derive(Debug, Eq, PartialEq)]
enum ProviderProbeOutcome {
    Exited { success: bool, stdout: Vec<u8> },
    TimedOut,
}

pub(crate) fn probe_agent_provider_statuses(
    config: AgentProviderProbeConfig<'_>,
) -> Vec<AgentProviderStatus> {
    let codex_readiness = config
        .codex_executable
        .map(|executable| probe_authentication(&config, executable, &["login", "status"], true))
        .unwrap_or(AgentProviderReadiness::ProviderMissing);
    let mut statuses = Vec::with_capacity(4);
    if config.codex_executable.is_some() {
        statuses.push(AgentProviderStatus::new(
            "codex",
            "codex-app-server-v2",
            codex_readiness,
        ));
    }
    for provider in config.acp_providers {
        let readiness = match provider.provider_id.as_str() {
            "codex-acp" => provider
                .environment
                .get("CODEX_PATH")
                .map(PathBuf::from)
                .as_deref()
                .map(|executable| {
                    probe_authentication(&config, executable, &["login", "status"], true)
                })
                .unwrap_or(AgentProviderReadiness::ProviderMissing),
            "claude-acp" => provider
                .environment
                .get("CLAUDE_CODE_EXECUTABLE")
                .map(PathBuf::from)
                .as_deref()
                .map(|executable| {
                    probe_authentication(&config, executable, &["auth", "status"], false)
                })
                .unwrap_or(AgentProviderReadiness::ProviderMissing),
            "copilot-acp" => probe_copilot(&provider.executable, &provider.environment),
            _ => continue,
        };
        statuses.push(AgentProviderStatus::new(
            &provider.provider_id,
            "acp-v1",
            readiness,
        ));
    }
    statuses
}

fn probe_authentication(
    config: &AgentProviderProbeConfig<'_>,
    executable: &Path,
    arguments: &[&str],
    codex: bool,
) -> AgentProviderReadiness {
    let mut environment = provider_probe_environment(config.provider_home, executable);
    if codex && let Some(codex_home) = config.codex_auth_file.and_then(Path::parent) {
        environment.insert("CODEX_HOME".into(), codex_home.as_os_str().to_owned());
    }
    match run_provider_probe(
        executable,
        arguments,
        Some(&environment),
        PROVIDER_PROBE_TIMEOUT,
    ) {
        Ok(ProviderProbeOutcome::Exited { success: true, .. }) => {
            AgentProviderReadiness::Authenticated
        }
        Ok(ProviderProbeOutcome::Exited { success: false, .. }) => {
            AgentProviderReadiness::LoginRequired
        }
        Ok(ProviderProbeOutcome::TimedOut) | Err(_) => AgentProviderReadiness::ProbeFailed,
    }
}

fn probe_copilot(
    executable: &Path,
    environment: &BTreeMap<String, OsString>,
) -> AgentProviderReadiness {
    match run_provider_probe(
        executable,
        &["--version"],
        Some(environment),
        PROVIDER_PROBE_TIMEOUT,
    ) {
        Ok(ProviderProbeOutcome::Exited {
            success: true,
            stdout,
        }) if String::from_utf8_lossy(&stdout).contains("GitHub Copilot CLI") => {
            // Copilot authenticates during its ACP session and does not expose
            // a read-only login status command.
            AgentProviderReadiness::Available
        }
        Ok(_) | Err(_) => AgentProviderReadiness::ProbeFailed,
    }
}

fn provider_probe_environment(home: &Path, executable: &Path) -> BTreeMap<String, OsString> {
    let mut path_entries = Vec::with_capacity(5);
    if let Some(parent) = executable.parent() {
        path_entries.push(parent.to_owned());
    }
    for entry in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        let path = PathBuf::from(entry);
        if !path_entries.contains(&path) {
            path_entries.push(path);
        }
    }
    let path =
        std::env::join_paths(path_entries).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"));
    let user = home
        .file_name()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::ffi::OsStr::new("user"))
        .to_owned();
    BTreeMap::from([
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), path),
        ("TERM".into(), "dumb".into()),
        ("USER".into(), user.clone()),
        ("LOGNAME".into(), user),
    ])
}

fn run_provider_probe(
    executable: &Path,
    arguments: &[&str],
    environment: Option<&BTreeMap<String, OsString>>,
    timeout: Duration,
) -> Result<ProviderProbeOutcome, String> {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Some(environment) = environment {
        command.env_clear().envs(environment);
    }
    let mut child = command.spawn().map_err(|error| {
        format!(
            "cannot start provider probe {}: {error}",
            executable.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "provider probe stdout is unavailable".to_owned())?;
    let reader = thread::spawn(move || {
        let mut stdout = stdout;
        let mut retained = Vec::with_capacity(PROVIDER_PROBE_MAX_STDOUT_BYTES);
        let mut buffer = [0_u8; 1024];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = PROVIDER_PROBE_MAX_STDOUT_BYTES.saturating_sub(retained.len());
                    retained.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                Err(_) => break,
            }
        }
        retained
    });
    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break ProviderProbeOutcome::Exited {
                    success: status.success(),
                    stdout: Vec::new(),
                };
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                terminate_provider_probe(&mut child);
                break ProviderProbeOutcome::TimedOut;
            }
            Err(error) => {
                terminate_provider_probe(&mut child);
                let _ = reader.join();
                return Err(format!(
                    "cannot inspect provider probe {}: {error}",
                    executable.display()
                ));
            }
        }
    };
    let retained = reader
        .join()
        .map_err(|_| "provider probe output reader panicked".to_owned())?;
    Ok(match outcome {
        ProviderProbeOutcome::Exited { success, .. } => ProviderProbeOutcome::Exited {
            success,
            stdout: retained,
        },
        ProviderProbeOutcome::TimedOut => ProviderProbeOutcome::TimedOut,
    })
}

fn terminate_provider_probe(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as i32);
        // SAFETY: the process group contains only the probe created above.
        unsafe { libc::kill(process_group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn provider_probe_timeout_reaps_its_process_group() {
        let temporary = tempfile::tempdir().expect("temporary provider");
        let executable = temporary.path().join("stuck-provider");
        std::fs::write(&executable, "#!/bin/sh\nsleep 5\n").expect("provider fixture");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("provider fixture permissions");
        let started = Instant::now();
        assert_eq!(
            run_provider_probe(&executable, &["--version"], None, Duration::from_millis(50))
                .expect("bounded probe"),
            ProviderProbeOutcome::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
