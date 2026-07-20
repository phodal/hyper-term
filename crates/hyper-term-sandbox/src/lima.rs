use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

use hyper_term_core::{
    SandboxCompileRequest, SandboxError, SandboxLaunchPlan, SandboxLauncher,
    canonicalize_sandbox_profile, canonicalize_terminal_command, sandbox_profile_digest,
    terminal_action_digest,
};
use hyper_term_protocol::{
    CompiledSandboxProfile, SandboxBackendKind, SandboxEnforcement, SandboxLifetime,
    SandboxNetworkPolicy,
};

use crate::{
    IsolatedChangeReport, IsolatedWorktree, IsolatedWorktreeError, IsolatedWorktreeManager,
};

const RECEIPT_SCHEMA_VERSION: u32 = 1;
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MIN_OUTPUT_BYTES: usize = 4 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

const GUEST_RUNNER: &str = r#"#!/bin/sh
set -eu
if [ "$#" -eq 0 ]; then
  exit 64
fi
uid="${SUDO_UID:?missing SUDO_UID}"
gid="${SUDO_GID:?missing SUDO_GID}"
user=$(awk -F: -v uid="$uid" '$3 == uid { print $1; exit }' /etc/passwd)
test -n "$user"
home="/tmp/hyper-term-$uid"
mkdir -p "$home"
chown "$uid:$gid" "$home"
chmod 0700 "$home"
ulimit -n 256
ulimit -u 256
exec unshare --net -- su "$user" -s /bin/sh -c 'home=$1; shift; exec env -i HOME="$home" TMPDIR=/tmp PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 "$@"' hyper-term "$home" "$@"
"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LimaImage {
    pub path: PathBuf,
    /// Lowercase, unprefixed SHA-256 of the local image.
    pub sha256: String,
    pub arch: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LimaRunnerConfig {
    pub image: LimaImage,
    pub vm_type: String,
    pub cpus: u8,
    pub memory_mib: u32,
    pub disk_gib: u16,
    pub start_timeout: Duration,
    pub task_timeout: Duration,
    pub max_output_bytes: usize,
}

impl LimaRunnerConfig {
    pub fn macos_vz(image: LimaImage) -> Self {
        Self {
            image,
            vm_type: "vz".into(),
            cpus: 2,
            memory_mib: 2_048,
            disk_gib: 8,
            start_timeout: Duration::from_secs(5 * 60),
            task_timeout: Duration::from_secs(10 * 60),
            max_output_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedTaskRequest {
    /// Exact argv passed to the guest; no host or guest shell interpolation is used.
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedTaskTermination {
    Exited,
    Signaled,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedTaskReceipt {
    pub schema_version: u32,
    pub environment_id: String,
    pub source_revision: String,
    pub backend: String,
    pub backend_version: String,
    pub image_sha256: String,
    pub command_sha256: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub termination: IsolatedTaskTermination,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub changes: IsolatedChangeReport,
    pub cleanup_complete: bool,
}

#[derive(Clone, Debug)]
pub struct LimaTaskRunner {
    limactl_executable: PathBuf,
    backend_version: String,
    config: LimaRunnerConfig,
}

/// Compiles the permission-broker contract for a command that must be routed
/// to [`LimaTaskRunner`], never to the host PTY launcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct LimaIsolatedTaskLauncher;

impl SandboxLauncher for LimaIsolatedTaskLauncher {
    fn compile(&self, request: &SandboxCompileRequest) -> Result<SandboxLaunchPlan, SandboxError> {
        let profile = canonicalize_sandbox_profile(&request.profile)?;
        if profile.enforcement != SandboxEnforcement::IsolatedTask
            || profile.network != SandboxNetworkPolicy::Offline
            || profile.lifetime != SandboxLifetime::OneOperation
        {
            return Err(SandboxError::Backend(
                "Lima requires isolated_task, offline, one_operation".into(),
            ));
        }
        let command = canonicalize_terminal_command(&request.command)?;
        Ok(SandboxLaunchPlan {
            compiled: CompiledSandboxProfile {
                backend: SandboxBackendKind::LimaVm,
                enforced: true,
                profile_digest: sandbox_profile_digest(&profile)?,
                action_digest: terminal_action_digest(&command)?,
                profile,
            },
            command,
            clear_environment: true,
        })
    }
}

impl LimaTaskRunner {
    pub fn task_timeout(&self) -> Duration {
        self.config.task_timeout
    }

    pub fn max_output_bytes(&self) -> usize {
        self.config.max_output_bytes
    }

    pub fn discover(config: LimaRunnerConfig) -> Result<Self, LimaRunnerError> {
        let executable = discover_limactl().ok_or(LimaRunnerError::LimaUnavailable)?;
        Self::with_executable(executable, config)
    }

    pub fn with_executable(
        executable: impl AsRef<Path>,
        config: LimaRunnerConfig,
    ) -> Result<Self, LimaRunnerError> {
        validate_config(&config)?;
        let limactl_executable = fs::canonicalize(executable.as_ref()).map_err(|error| {
            LimaRunnerError::InvalidExecutable {
                path: executable.as_ref().to_path_buf(),
                error,
            }
        })?;
        if !fs::metadata(&limactl_executable)?.is_file() {
            return Err(LimaRunnerError::ExecutableNotFile(limactl_executable));
        }
        let output = Command::new(&limactl_executable)
            .env_clear()
            .env("HOME", "/var/empty")
            .env("LC_ALL", "C")
            .arg("--version")
            .output()?;
        if !output.status.success() || output.stdout.len() > 4_096 || output.stderr.len() > 4_096 {
            return Err(LimaRunnerError::VersionProbeFailed);
        }
        let backend_version = String::from_utf8(output.stdout)
            .map_err(|_| LimaRunnerError::VersionProbeFailed)?
            .trim()
            .to_string();
        if !backend_version.starts_with("limactl version ") {
            return Err(LimaRunnerError::VersionProbeFailed);
        }
        Ok(Self {
            limactl_executable,
            backend_version,
            config,
        })
    }

    /// Executes one exact-commit worktree in a private, ephemeral Lima home.
    ///
    /// The VM receives exactly one writable host mount. The task itself is
    /// wrapped by Linux `unshare --net`, so it cannot use the VM's boot network.
    /// Cleanup is mandatory; a cleanup failure suppresses the otherwise valid
    /// task result and fails closed.
    pub fn run(
        &self,
        manager: &IsolatedWorktreeManager,
        environment: &IsolatedWorktree,
        request: &IsolatedTaskRequest,
        cancelled: &AtomicBool,
    ) -> Result<IsolatedTaskReceipt, LimaRunnerError> {
        validate_request(request)?;
        let lima_home = create_private_lima_home(&environment.manifest.environment_id)?;
        let staged_image = environment.environment_root.join("pinned-vm-image");
        stage_image(&self.config.image, &staged_image)?;
        let config_path = environment.environment_root.join("lima.yaml");
        let receipt_path = environment.environment_root.join("task-receipt.json");
        let instance_name = instance_name(&environment.manifest.environment_id);
        write_private_json(
            &config_path,
            &compile_lima_config(&self.config, environment, &staged_image)?,
        )?;

        let started_at_ms = unix_time_ms()?;
        let mut started = false;
        let task_result = (|| {
            self.control(
                &lima_home,
                [
                    "--tty=false",
                    "validate",
                    config_path.to_str().unwrap_or_default(),
                ],
                self.config.start_timeout,
            )?;
            let start_timeout = format!("{}s", self.config.start_timeout.as_secs().max(1));
            self.control_owned(
                &lima_home,
                vec![
                    "--tty=false".into(),
                    "start".into(),
                    "--name".into(),
                    instance_name.clone(),
                    "--timeout".into(),
                    start_timeout,
                    config_path.to_string_lossy().into_owned(),
                ],
                self.config.start_timeout,
            )?;
            started = true;
            let command_sha256 = command_digest(&request.argv);
            let execution =
                self.execute_task(&lima_home, &instance_name, &request.argv, cancelled)?;
            let changes = manager.inspect_changes(environment)?;
            let finished_at_ms = unix_time_ms()?;
            Ok(IsolatedTaskReceipt {
                schema_version: RECEIPT_SCHEMA_VERSION,
                environment_id: environment.manifest.environment_id.clone(),
                source_revision: environment.manifest.source_revision.clone(),
                backend: "lima".into(),
                backend_version: self.backend_version.clone(),
                image_sha256: self.config.image.sha256.clone(),
                command_sha256,
                started_at_ms,
                finished_at_ms,
                termination: execution.termination,
                exit_code: execution.status.and_then(|status| status.code()),
                stdout_sha256: hex_digest(execution.stdout.as_bytes()),
                stderr_sha256: hex_digest(execution.stderr.as_bytes()),
                stdout: execution.stdout,
                stderr: execution.stderr,
                changes,
                cleanup_complete: true,
            })
        })();

        let cleanup = self.cleanup(&lima_home, &instance_name, started);
        if cleanup.is_ok() {
            fs::remove_dir_all(&lima_home)?;
        }
        match (task_result, cleanup) {
            (Ok(receipt), Ok(())) => {
                write_private_json(&receipt_path, &receipt)?;
                Ok(receipt)
            }
            (Err(error), Ok(())) => Err(error),
            (_, Err(cleanup)) => Err(cleanup),
        }
    }

    fn execute_task(
        &self,
        lima_home: &Path,
        instance_name: &str,
        argv: &[String],
        cancelled: &AtomicBool,
    ) -> Result<Execution, LimaRunnerError> {
        let mut command = self.command(lima_home);
        command
            .arg("--tty=false")
            .arg("shell")
            .arg("--workdir")
            .arg("/workspace")
            .arg(instance_name)
            .arg("--")
            .arg("sudo")
            .arg("-n")
            .arg("/usr/local/bin/hyper-term-isolated-exec")
            .args(argv);
        run_bounded(
            command,
            self.config.task_timeout,
            self.config.max_output_bytes,
            Some(cancelled),
        )
    }

    fn cleanup(
        &self,
        lima_home: &Path,
        instance_name: &str,
        started: bool,
    ) -> Result<(), LimaRunnerError> {
        if started {
            self.control(
                lima_home,
                ["--tty=false", "stop", "--force", instance_name],
                CLEANUP_TIMEOUT,
            )?;
        }
        let result = self.control(
            lima_home,
            ["--tty=false", "delete", "--force", instance_name],
            CLEANUP_TIMEOUT,
        );
        if !started && matches!(result, Err(LimaRunnerError::ControlFailed { .. })) {
            return Ok(());
        }
        result
    }

    fn control<const N: usize>(
        &self,
        lima_home: &Path,
        arguments: [&str; N],
        timeout: Duration,
    ) -> Result<(), LimaRunnerError> {
        self.control_owned(
            lima_home,
            arguments.into_iter().map(str::to_owned).collect(),
            timeout,
        )
    }

    fn control_owned(
        &self,
        lima_home: &Path,
        arguments: Vec<String>,
        timeout: Duration,
    ) -> Result<(), LimaRunnerError> {
        let mut command = self.command(lima_home);
        command.args(&arguments);
        let result = run_bounded(command, timeout, 256 * 1024, None)?;
        if result.termination != IsolatedTaskTermination::Exited
            || !result.status.is_some_and(|status| status.success())
        {
            return Err(LimaRunnerError::ControlFailed {
                command: arguments.first().cloned().unwrap_or_default(),
                status: result.status.and_then(|status| status.code()),
                stderr: result.stderr,
            });
        }
        Ok(())
    }

    fn command(&self, lima_home: &Path) -> Command {
        let mut command = Command::new(&self.limactl_executable);
        command
            .env_clear()
            .env("HOME", lima_home)
            .env("LIMA_HOME", lima_home)
            .env("LC_ALL", "C")
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .stdin(Stdio::null());
        command
    }
}

pub fn read_isolated_task_receipt(
    environment: &IsolatedWorktree,
) -> Result<IsolatedTaskReceipt, LimaRunnerError> {
    let path = environment.environment_root.join("task-receipt.json");
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 8 * 1024 * 1024
    {
        return Err(LimaRunnerError::InvalidReceipt);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(LimaRunnerError::InvalidReceipt);
        }
    }
    let receipt: IsolatedTaskReceipt = serde_json::from_reader(File::open(path)?)?;
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.environment_id != environment.manifest.environment_id
        || receipt.source_revision != environment.manifest.source_revision
        || receipt.backend != "lima"
        || !receipt.cleanup_complete
        || !valid_sha256(&receipt.image_sha256)
        || !valid_sha256(&receipt.command_sha256)
        || !valid_sha256(&receipt.stdout_sha256)
        || !valid_sha256(&receipt.stderr_sha256)
        || receipt.stdout_sha256 != hex_digest(receipt.stdout.as_bytes())
        || receipt.stderr_sha256 != hex_digest(receipt.stderr.as_bytes())
        || receipt.changes.environment_id != receipt.environment_id
        || receipt.changes.source_revision != receipt.source_revision
    {
        return Err(LimaRunnerError::InvalidReceipt);
    }
    Ok(receipt)
}

#[derive(Debug)]
struct Execution {
    status: Option<ExitStatus>,
    termination: IsolatedTaskTermination,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Error)]
pub enum LimaRunnerError {
    #[error("Lima is unavailable")]
    LimaUnavailable,
    #[error("cannot resolve Lima executable {path}: {error}")]
    InvalidExecutable {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("Lima executable is not a regular file: {0}")]
    ExecutableNotFile(PathBuf),
    #[error("Lima version probe failed")]
    VersionProbeFailed,
    #[error("invalid Lima runner configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid isolated task command")]
    InvalidCommand,
    #[error("local VM image exceeds the {MAX_IMAGE_BYTES} byte bound")]
    ImageTooLarge,
    #[error("local VM image digest does not match its pinned SHA-256")]
    ImageDigestMismatch,
    #[error("isolated task receipt failed integrity validation")]
    InvalidReceipt,
    #[error("private Lima home is insecure: {0}")]
    InsecureLimaHome(PathBuf),
    #[error("Lima control command {command} failed with status {status:?}: {stderr}")]
    ControlFailed {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    #[error("sandbox process output exceeded the configured bound")]
    OutputTooLarge,
    #[error("sandbox process reader failed")]
    ReaderFailed,
    #[error("system clock is before the Unix epoch")]
    InvalidClock,
    #[error(transparent)]
    Worktree(#[from] IsolatedWorktreeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn validate_config(config: &LimaRunnerConfig) -> Result<(), LimaRunnerError> {
    if config.vm_type != "vz"
        || !matches!(config.image.arch.as_str(), "aarch64" | "x86_64")
        || config.cpus == 0
        || config.cpus > 8
        || !(512..=16_384).contains(&config.memory_mib)
        || !(2..=128).contains(&config.disk_gib)
        || config.start_timeout.is_zero()
        || config.task_timeout.is_zero()
        || !(MIN_OUTPUT_BYTES..=MAX_OUTPUT_BYTES).contains(&config.max_output_bytes)
        || !valid_sha256(&config.image.sha256)
    {
        return Err(LimaRunnerError::InvalidConfig(
            "use VZ, a supported architecture, bounded resources, and a lowercase SHA-256".into(),
        ));
    }
    Ok(())
}

fn validate_request(request: &IsolatedTaskRequest) -> Result<(), LimaRunnerError> {
    if request.argv.is_empty()
        || request.argv.len() > MAX_ARGUMENTS
        || request.argv.iter().any(|value| {
            value.is_empty() || value.as_bytes().contains(&0) || value.len() > MAX_ARGUMENT_BYTES
        })
        || request.argv.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
    {
        return Err(LimaRunnerError::InvalidCommand);
    }
    Ok(())
}

fn stage_image(image: &LimaImage, destination: &Path) -> Result<(), LimaRunnerError> {
    let path = fs::canonicalize(&image.path)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(LimaRunnerError::InvalidConfig(
            "VM image must be a regular local file".into(),
        ));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(LimaRunnerError::ImageTooLarge);
    }
    let mut file = File::open(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut staged = options.open(destination)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        staged.write_all(&buffer[..read])?;
    }
    if encode_hex(digest.finalize().as_slice()) != image.sha256 {
        drop(staged);
        let _ = fs::remove_file(destination);
        return Err(LimaRunnerError::ImageDigestMismatch);
    }
    staged.sync_all()?;
    Ok(())
}

fn compile_lima_config(
    config: &LimaRunnerConfig,
    environment: &IsolatedWorktree,
    image_path: &Path,
) -> Result<serde_json::Value, LimaRunnerError> {
    let image = fs::canonicalize(image_path)?;
    let worktree = fs::canonicalize(&environment.manifest.worktree)?;
    Ok(json!({
        "minimumLimaVersion": "2.1.1",
        "vmType": config.vm_type,
        "arch": config.image.arch,
        "images": [{
            "location": image,
            "arch": config.image.arch,
            "digest": format!("sha256:{}", config.image.sha256),
        }],
        "cpus": config.cpus,
        "memory": format!("{}MiB", config.memory_mib),
        "disk": format!("{}GiB", config.disk_gib),
        "mountType": "virtiofs",
        "mounts": [{
            "location": worktree,
            "mountPoint": "/workspace",
            "writable": true,
        }],
        "containerd": {"system": false, "user": false},
        "propagateProxyEnv": false,
        "hostResolver": {"enabled": false},
        "dns": [],
        "ssh": {
            "loadDotSSHPubKeys": false,
            "forwardAgent": false,
            "forwardX11": false,
            "forwardX11Trusted": false,
        },
        "portForwards": [{
            "guestIP": "0.0.0.0",
            "proto": "any",
            "guestPortRange": [1, 65535],
            "ignore": true,
        }],
        "provision": [{
            "mode": "system",
            "script": format!(
                "#!/bin/sh\nset -eu\ninstall -d -m 0755 /usr/local/bin\ncat > /usr/local/bin/hyper-term-isolated-exec <<'HYPER_TERM_RUNNER'\n{}HYPER_TERM_RUNNER\nchmod 0755 /usr/local/bin/hyper-term-isolated-exec\n",
                GUEST_RUNNER
            ),
        }],
    }))
}

fn run_bounded(
    mut command: Command,
    timeout: Duration,
    output_limit: usize,
    cancelled: Option<&AtomicBool>,
) -> Result<Execution, LimaRunnerError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or(LimaRunnerError::ReaderFailed)?;
    let stderr = child.stderr.take().ok_or(LimaRunnerError::ReaderFailed)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_reader(stdout, output_limit, Arc::clone(&overflow));
    let stderr_reader = spawn_reader(stderr, output_limit, Arc::clone(&overflow));
    let started = Instant::now();
    let (status, termination) = loop {
        if overflow.load(Ordering::Relaxed) {
            terminate(&mut child)?;
            break (child.wait().ok(), IsolatedTaskTermination::Signaled);
        }
        if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            terminate(&mut child)?;
            break (child.wait().ok(), IsolatedTaskTermination::Cancelled);
        }
        if started.elapsed() >= timeout {
            terminate(&mut child)?;
            break (child.wait().ok(), IsolatedTaskTermination::TimedOut);
        }
        if let Some(status) = child.try_wait()? {
            let termination = if status.code().is_some() {
                IsolatedTaskTermination::Exited
            } else {
                IsolatedTaskTermination::Signaled
            };
            break (Some(status), termination);
        }
        thread::sleep(POLL_INTERVAL);
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| LimaRunnerError::ReaderFailed)??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| LimaRunnerError::ReaderFailed)??;
    if overflow.load(Ordering::Relaxed) {
        return Err(LimaRunnerError::OutputTooLarge);
    }
    Ok(Execution {
        status,
        termination,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
    overflow: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<Vec<u8>, std::io::Error>> {
    thread::spawn(move || {
        let mut output = Vec::with_capacity(limit.min(64 * 1024));
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(read) > limit {
                overflow.store(true, Ordering::Relaxed);
            } else {
                output.extend_from_slice(&buffer[..read]);
            }
        }
    })
}

fn terminate(child: &mut Child) -> Result<(), std::io::Error> {
    match child.kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_private_json(path: &Path, value: &impl Serialize) -> Result<(), LimaRunnerError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn create_private_lima_home(environment_id: &str) -> Result<PathBuf, LimaRunnerError> {
    let root = PathBuf::from("/tmp/ht-lima");
    if root.exists() {
        let metadata = fs::symlink_metadata(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(LimaRunnerError::InsecureLimaHome(root));
            }
        }
    } else {
        create_private_directory(&root)?;
    }
    let digest = hex_digest(environment_id.as_bytes());
    let home = root.join(&digest[..16]);
    create_private_directory(&home)?;
    Ok(home)
}

fn command_digest(argv: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"hyper-term-isolated-command-v1\0");
    for argument in argv {
        digest.update((argument.len() as u64).to_le_bytes());
        digest.update(argument.as_bytes());
    }
    encode_hex(digest.finalize().as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    encode_hex(Sha256::digest(bytes).as_slice())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn instance_name(environment_id: &str) -> String {
    let digest = hex_digest(environment_id.as_bytes());
    format!("ht-{}", &digest[..12])
}

fn unix_time_ms() -> Result<u64, LimaRunnerError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LimaRunnerError::InvalidClock)?
        .as_millis() as u64)
}

fn discover_limactl() -> Option<PathBuf> {
    [
        "/opt/homebrew/bin/limactl",
        "/usr/local/bin/limactl",
        "/usr/bin/limactl",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IsolatedWorktreeRequest;

    fn run_git(cwd: &Path, arguments: &[&str]) {
        let status = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(cwd)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn isolated_environment() -> (tempfile::TempDir, IsolatedWorktreeManager, IsolatedWorktree) {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        run_git(&repository, &["init", "-q"]);
        run_git(&repository, &["config", "user.name", "Hyper Term Test"]);
        run_git(
            &repository,
            &["config", "user.email", "hyper-term@example.invalid"],
        );
        fs::write(repository.join("README.md"), "source\n").unwrap();
        run_git(&repository, &["add", "."]);
        run_git(&repository, &["commit", "-qm", "fixture"]);
        let manager = IsolatedWorktreeManager::discover().unwrap();
        let environment = manager
            .create(&IsolatedWorktreeRequest {
                source_workspace: repository,
                state_root: temporary.path().join("state"),
                task_id: "lima-task".into(),
                revision: None,
            })
            .unwrap();
        (temporary, manager, environment)
    }

    #[cfg(unix)]
    fn fake_limactl(root: &Path, worktree: &Path, shell_body: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let executable = root.join("limactl");
        let log = root.join("limactl.log");
        let script = format!(
            "#!/bin/sh\nset -eu\nif [ \"${{1:-}}\" = \"--version\" ]; then echo 'limactl version 2.1.1'; exit 0; fi\naction=''\nfor argument in \"$@\"; do\n  case \"$argument\" in validate|start|shell|stop|delete) action=\"$argument\"; break;; esac\ndone\nprintf '%s\\n' \"$action\" >> '{}'\ncase \"$action\" in\n  shell) {};;\n  *) exit 0;;\nesac\n",
            log.display(),
            shell_body.replace("$WORKTREE", &worktree.to_string_lossy())
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        (executable, log)
    }

    fn runner_config(root: &Path) -> LimaRunnerConfig {
        let image = root.join("image.qcow2");
        fs::write(&image, b"pinned local image").unwrap();
        LimaRunnerConfig {
            image: LimaImage {
                path: image,
                sha256: hex_digest(b"pinned local image"),
                arch: "aarch64".into(),
            },
            vm_type: "vz".into(),
            cpus: 2,
            memory_mib: 1_024,
            disk_gib: 4,
            start_timeout: Duration::from_secs(2),
            task_timeout: Duration::from_secs(2),
            max_output_bytes: MIN_OUTPUT_BYTES,
        }
    }

    #[test]
    fn rejects_unpinned_or_overpowered_configuration() {
        let config = LimaRunnerConfig {
            image: LimaImage {
                path: PathBuf::from("image"),
                sha256: "not-a-digest".into(),
                arch: "aarch64".into(),
            },
            vm_type: "vz".into(),
            cpus: 64,
            memory_mib: 2_048,
            disk_gib: 8,
            start_timeout: Duration::from_secs(1),
            task_timeout: Duration::from_secs(1),
            max_output_bytes: MIN_OUTPUT_BYTES,
        };
        assert!(matches!(
            validate_config(&config),
            Err(LimaRunnerError::InvalidConfig(_))
        ));
    }

    #[test]
    fn command_digest_preserves_argument_boundaries() {
        assert_ne!(
            command_digest(&["ab".into(), "c".into()]),
            command_digest(&["a".into(), "bc".into()])
        );
    }

    #[test]
    fn bounded_execution_distinguishes_timeout_and_cancellation() {
        let mut timeout = Command::new("/bin/sh");
        timeout.args(["-c", "while :; do :; done"]);
        let result =
            run_bounded(timeout, Duration::from_millis(50), MIN_OUTPUT_BYTES, None).unwrap();
        assert_eq!(result.termination, IsolatedTaskTermination::TimedOut);

        let cancelled = AtomicBool::new(true);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "while :; do :; done"]);
        let result = run_bounded(
            command,
            Duration::from_secs(1),
            MIN_OUTPUT_BYTES,
            Some(&cancelled),
        )
        .unwrap();
        assert_eq!(result.termination, IsolatedTaskTermination::Cancelled);
    }

    #[test]
    fn bounded_execution_rejects_output_floods() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "yes x | head -c 8192"]);
        assert!(matches!(
            run_bounded(command, Duration::from_secs(1), MIN_OUTPUT_BYTES, None),
            Err(LimaRunnerError::OutputTooLarge)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_records_reviewable_changes_and_cleans_up() {
        let (temporary, manager, environment) = isolated_environment();
        let (executable, log) = fake_limactl(
            temporary.path(),
            &environment.manifest.worktree,
            "printf 'generated\\n' > '$WORKTREE/generated.txt'; printf 'streamed output\\n'; exit 0",
        );
        let runner =
            LimaTaskRunner::with_executable(executable, runner_config(temporary.path())).unwrap();
        let receipt = runner
            .run(
                &manager,
                &environment,
                &IsolatedTaskRequest {
                    argv: vec!["/bin/sh".into(), "-c".into(), "printf safe".into()],
                },
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(receipt.termination, IsolatedTaskTermination::Exited);
        assert_eq!(receipt.exit_code, Some(0));
        assert_eq!(receipt.stdout, "streamed output\n");
        assert!(receipt.cleanup_complete);
        assert_eq!(receipt.changes.changed_files.len(), 1);
        assert_eq!(
            receipt.changes.changed_files[0].path,
            Path::new("generated.txt")
        );
        assert!(
            environment
                .environment_root
                .join("task-receipt.json")
                .is_file()
        );
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            "validate\nstart\nshell\nstop\ndelete\n"
        );
        manager.destroy(&environment).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_task_exit_is_a_receipt_and_still_cleans_up() {
        let (temporary, manager, environment) = isolated_environment();
        let (executable, log) = fake_limactl(
            temporary.path(),
            &environment.manifest.worktree,
            "printf 'task failed' >&2; exit 23",
        );
        let runner =
            LimaTaskRunner::with_executable(executable, runner_config(temporary.path())).unwrap();
        let receipt = runner
            .run(
                &manager,
                &environment,
                &IsolatedTaskRequest {
                    argv: vec!["/bin/false".into()],
                },
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(receipt.termination, IsolatedTaskTermination::Exited);
        assert_eq!(receipt.exit_code, Some(23));
        assert_eq!(receipt.stderr, "task failed");
        assert!(fs::read_to_string(log).unwrap().ends_with("stop\ndelete\n"));
        manager.destroy(&environment).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_stops_and_deletes_the_ephemeral_instance() {
        let (temporary, manager, environment) = isolated_environment();
        let (executable, log) = fake_limactl(
            temporary.path(),
            &environment.manifest.worktree,
            "while :; do :; done",
        );
        let runner =
            LimaTaskRunner::with_executable(executable, runner_config(temporary.path())).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::Relaxed);
        });
        let receipt = runner
            .run(
                &manager,
                &environment,
                &IsolatedTaskRequest {
                    argv: vec!["/bin/true".into()],
                },
                &cancelled,
            )
            .unwrap();
        canceller.join().unwrap();
        assert_eq!(receipt.termination, IsolatedTaskTermination::Cancelled);
        assert!(fs::read_to_string(log).unwrap().ends_with("stop\ndelete\n"));
        manager.destroy(&environment).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_lima_accepts_the_generated_schema_without_starting_a_vm() {
        let limactl = Path::new("/opt/homebrew/bin/limactl");
        if !limactl.is_file() {
            return;
        }
        let (temporary, manager, environment) = isolated_environment();
        let config = runner_config(temporary.path());
        let config_path = environment.environment_root.join("schema.yaml");
        write_private_json(
            &config_path,
            &compile_lima_config(&config, &environment, &config.image.path).unwrap(),
        )
        .unwrap();
        let lima_home = environment.environment_root.join("schema-lima-home");
        create_private_directory(&lima_home).unwrap();
        let output = Command::new(limactl)
            .env_clear()
            .env("HOME", &lima_home)
            .env("LIMA_HOME", &lima_home)
            .args(["--tty=false", "validate"])
            .arg(&config_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        manager.destroy(&environment).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires HYPER_TERM_LIMA_IMAGE and HYPER_TERM_LIMA_IMAGE_SHA256"]
    fn real_lima_runs_an_offline_unprivileged_exact_commit_task() {
        let image = PathBuf::from(std::env::var("HYPER_TERM_LIMA_IMAGE").unwrap());
        let image_sha256 = std::env::var("HYPER_TERM_LIMA_IMAGE_SHA256").unwrap();
        let (_temporary, manager, environment) = isolated_environment();
        let runner = LimaTaskRunner::discover(LimaRunnerConfig {
            image: LimaImage {
                path: image,
                sha256: image_sha256,
                arch: "aarch64".into(),
            },
            vm_type: "vz".into(),
            cpus: 2,
            memory_mib: 1_024,
            disk_gib: 4,
            start_timeout: Duration::from_secs(5 * 60),
            task_timeout: Duration::from_secs(2 * 60),
            max_output_bytes: 64 * 1024,
        })
        .unwrap();
        let receipt = runner
            .run(
                &manager,
                &environment,
                &IsolatedTaskRequest {
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "set -eu; test \"$(id -u)\" != 0; printf 'real lima\\n' > generated.txt; if wget -q -T 3 -O /tmp/network-probe https://example.com; then exit 91; fi; printf 'offline unprivileged\\n'"
                            .into(),
                    ],
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(receipt.termination, IsolatedTaskTermination::Exited);
        assert_eq!(receipt.exit_code, Some(0), "{}", receipt.stderr);
        assert_eq!(receipt.stdout, "offline unprivileged\n");
        assert!(
            receipt
                .changes
                .changed_files
                .iter()
                .any(|change| change.path == Path::new("generated.txt"))
        );
        manager.destroy(&environment).unwrap();
    }
}
