use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use hyper_term_core::{SandboxLaunchPlan, terminal_action_digest};
use hyper_term_protocol::{SandboxBackendKind, TerminalCommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{DEFAULT_MAX_DRIVER_FRAME_BYTES, DriverFraming};

const DRIVER_EVENT_CAPACITY: usize = 256;
const STDERR_TAIL_BYTES: usize = 64 * 1024;
const EXIT_AFTER_STDOUT_GRACE: Duration = Duration::from_millis(200);
const EFFECT_TIMEOUT_GRACE: Duration = Duration::from_millis(100);
const MAX_SANDBOX_ARGUMENT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_DRIVER_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    DenoLsp,
    DenoGenUi,
    AcpAgent,
    CodexAppServer,
    ClaudeStreamJson,
    McpServer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverState {
    Starting,
    Ready,
    Busy,
    Waiting,
    Closing,
    Closed,
    Failed,
    UnknownExecution,
}

impl DriverState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed | Self::UnknownExecution)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DriverManifest {
    pub driver_id: Uuid,
    pub kind: DriverKind,
    pub implementation_version: String,
    pub protocol_version: String,
    pub capabilities: Vec<String>,
    pub transport: String,
    pub executable_sha256: String,
    pub permission_profile: String,
}

pub struct DriverSpec {
    pub manifest: DriverManifest,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub environment: BTreeMap<String, OsString>,
    /// A Rust-compiled, enforced launch plan for the exact audited command.
    /// The supervisor re-computes the inner command digest before using it.
    pub sandbox: Option<SandboxLaunchPlan>,
    pub framing: DriverFraming,
    pub max_frame_bytes: usize,
    /// Maximum decoded payload bytes retained in the supervisor event queue.
    /// The budget is released only when the consumer receives an event.
    pub max_pending_output_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DriverEvent {
    Message {
        sequence: u64,
        payload: Value,
    },
    ProtocolError {
        message: String,
    },
    Exited {
        code: Option<i32>,
        state: DriverState,
    },
}

pub(crate) struct BoundedDriverInbox {
    events: VecDeque<(DriverEvent, usize)>,
    output_bytes: usize,
    max_events: usize,
    max_output_bytes: usize,
}

impl BoundedDriverInbox {
    pub fn new(max_events: usize, max_output_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            output_bytes: 0,
            max_events,
            max_output_bytes,
        }
    }

    pub fn push_back(&mut self, event: DriverEvent) -> Result<(), DriverEvent> {
        let output_bytes = driver_event_output_bytes(&event);
        let Some(next_output_bytes) = self.output_bytes.checked_add(output_bytes) else {
            return Err(event);
        };
        if self.events.len() == self.max_events || next_output_bytes > self.max_output_bytes {
            return Err(event);
        }
        self.events.push_back((event, output_bytes));
        self.output_bytes = next_output_bytes;
        Ok(())
    }

    pub fn pop_front(&mut self) -> Option<DriverEvent> {
        let (event, output_bytes) = self.events.pop_front()?;
        self.output_bytes = self.output_bytes.saturating_sub(output_bytes);
        Some(event)
    }
}

fn driver_event_output_bytes(event: &DriverEvent) -> usize {
    match event {
        DriverEvent::Message { payload, .. } => serde_json::to_vec(payload)
            .map(|payload| payload.len())
            .unwrap_or(usize::MAX),
        DriverEvent::ProtocolError { message } => message.len(),
        DriverEvent::Exited { .. } => 0,
    }
}

pub struct DriverProcess {
    manifest: DriverManifest,
    framing: DriverFraming,
    max_frame_bytes: usize,
    input: Mutex<Option<ChildStdin>>,
    shared: Arc<DriverShared>,
    events: Receiver<QueuedDriverEvent>,
}

struct QueuedDriverEvent {
    event: DriverEvent,
    output_bytes: usize,
}

struct DriverShared {
    child: Mutex<Child>,
    process_group_id: u32,
    state: Mutex<DriverState>,
    stderr: Mutex<VecDeque<u8>>,
    pending_output_bytes: AtomicUsize,
    effect_in_flight: AtomicBool,
    exit_reported: AtomicBool,
}

impl DriverProcess {
    pub fn spawn(spec: DriverSpec) -> Result<Self, DriverError> {
        validate_spec(&spec)?;
        let executable = spec.executable.canonicalize()?;
        let actual_digest = sha256_file(&executable)?;
        if actual_digest != spec.manifest.executable_sha256 {
            return Err(DriverError::ExecutableDigestMismatch {
                expected: spec.manifest.executable_sha256,
                actual: actual_digest,
            });
        }
        let working_directory = spec.working_directory.canonicalize()?;
        let launch = resolve_launch(&spec, &executable, &working_directory)?;
        let mut command = Command::new(launch.executable);
        command
            .args(launch.arguments)
            .current_dir(launch.working_directory)
            .env_clear()
            .envs(launch.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command.spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or(DriverError::MissingPipe("stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or(DriverError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(DriverError::MissingPipe("stderr"))?;
        let process_group_id = child.id();
        let shared = Arc::new(DriverShared {
            child: Mutex::new(child),
            process_group_id,
            state: Mutex::new(DriverState::Starting),
            stderr: Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)),
            pending_output_bytes: AtomicUsize::new(0),
            effect_in_flight: AtomicBool::new(false),
            exit_reported: AtomicBool::new(false),
        });
        let (event_tx, events) = bounded(DRIVER_EVENT_CAPACITY);

        if let Err(error) = spawn_stderr_reader(Arc::clone(&shared), stderr) {
            let _ = terminate_process(&shared, Duration::from_millis(100));
            return Err(error);
        }
        if let Err(error) = spawn_protocol_reader(
            Arc::clone(&shared),
            output,
            spec.framing,
            spec.max_frame_bytes,
            spec.max_pending_output_bytes,
            event_tx,
        ) {
            let _ = terminate_process(&shared, Duration::from_millis(100));
            return Err(error);
        }

        Ok(Self {
            manifest: spec.manifest,
            framing: spec.framing,
            max_frame_bytes: spec.max_frame_bytes,
            input: Mutex::new(Some(input)),
            shared,
            events,
        })
    }

    pub fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    pub fn state(&self) -> Result<DriverState, DriverError> {
        Ok(*lock(&self.shared.state)?)
    }

    pub fn mark_ready(&self) -> Result<(), DriverError> {
        self.transition(DriverState::Starting, DriverState::Ready)
    }

    pub fn begin_effect(&self) -> Result<(), DriverError> {
        self.transition(DriverState::Ready, DriverState::Busy)?;
        self.shared.effect_in_flight.store(true, Ordering::Release);
        Ok(())
    }

    pub fn finish_effect(&self) -> Result<(), DriverError> {
        self.transition(DriverState::Busy, DriverState::Ready)?;
        self.shared.effect_in_flight.store(false, Ordering::Release);
        Ok(())
    }

    pub fn mark_waiting(&self) -> Result<(), DriverError> {
        self.transition(DriverState::Busy, DriverState::Waiting)
    }

    pub fn resolve_waiting(&self) -> Result<(), DriverError> {
        self.transition(DriverState::Waiting, DriverState::Ready)?;
        self.shared.effect_in_flight.store(false, Ordering::Release);
        Ok(())
    }

    pub fn send_json(&self, payload: &Value) -> Result<(), DriverError> {
        let state = self.state()?;
        if state.is_terminal() || state == DriverState::Closing {
            return Err(DriverError::IllegalState {
                expected: "a live driver",
                actual: state,
            });
        }
        let mut input = lock(&self.input)?;
        let writer = input.as_mut().ok_or(DriverError::InputClosed)?;
        self.framing.write(writer, payload, self.max_frame_bytes)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<DriverEvent, DriverError> {
        match self.events.recv_timeout(timeout) {
            Ok(queued) => {
                if queued.output_bytes > 0 {
                    self.shared
                        .pending_output_bytes
                        .fetch_sub(queued.output_bytes, Ordering::AcqRel);
                }
                Ok(queued.event)
            }
            Err(RecvTimeoutError::Timeout)
                if self.shared.effect_in_flight.load(Ordering::Acquire) =>
            {
                let state = self.stop(EFFECT_TIMEOUT_GRACE)?;
                Err(DriverError::EffectTimedOut { state })
            }
            Err(RecvTimeoutError::Timeout) => Err(DriverError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(DriverError::EventStreamClosed),
        }
    }

    pub fn stderr_tail(&self) -> Result<String, DriverError> {
        let mut bytes = lock(&self.shared.stderr)?;
        Ok(String::from_utf8_lossy(bytes.make_contiguous()).into_owned())
    }

    pub fn stop(&self, grace: Duration) -> Result<DriverState, DriverError> {
        let current = self.state()?;
        if current.is_terminal() {
            return Ok(current);
        }
        *lock(&self.shared.state)? = DriverState::Closing;
        lock(&self.input)?.take();
        // Keep the flag set until the protocol reader observes process exit so
        // its Exited event cannot race this call and incorrectly report Closed.
        let uncertain = self.shared.effect_in_flight.load(Ordering::Acquire);
        let status = terminate_process(&self.shared, grace)?;
        let final_state = if uncertain {
            DriverState::UnknownExecution
        } else if status.is_some() {
            DriverState::Closed
        } else {
            DriverState::Failed
        };
        *lock(&self.shared.state)? = final_state;
        Ok(final_state)
    }

    fn transition(&self, expected: DriverState, next: DriverState) -> Result<(), DriverError> {
        let mut state = lock(&self.shared.state)?;
        if *state != expected {
            return Err(DriverError::IllegalState {
                expected: match expected {
                    DriverState::Starting => "starting",
                    DriverState::Ready => "ready",
                    DriverState::Busy => "busy",
                    DriverState::Waiting => "waiting",
                    DriverState::Closing => "closing",
                    DriverState::Closed => "closed",
                    DriverState::Failed => "failed",
                    DriverState::UnknownExecution => "unknown execution",
                },
                actual: *state,
            });
        }
        *state = next;
        Ok(())
    }
}

struct ResolvedDriverLaunch {
    executable: PathBuf,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    environment: BTreeMap<String, OsString>,
}

fn resolve_launch(
    spec: &DriverSpec,
    executable: &Path,
    working_directory: &Path,
) -> Result<ResolvedDriverLaunch, DriverError> {
    let Some(plan) = &spec.sandbox else {
        return Ok(ResolvedDriverLaunch {
            executable: executable.to_path_buf(),
            arguments: spec.arguments.clone(),
            working_directory: working_directory.to_path_buf(),
            environment: spec.environment.clone(),
        });
    };
    if !plan.compiled.enforced || !plan.clear_environment {
        return Err(DriverError::InvalidContainment(
            "driver sandbox plan must be enforced and clear the inherited environment".into(),
        ));
    }
    let audited = audited_command(spec, executable, working_directory)?;
    let actual_digest = terminal_action_digest(&audited)
        .map_err(|error| DriverError::InvalidContainment(error.to_string()))?;
    if actual_digest != plan.compiled.action_digest {
        return Err(DriverError::SandboxActionDigestMismatch {
            expected: plan.compiled.action_digest.to_string(),
            actual: actual_digest.to_string(),
        });
    }
    let expected_profile = sandbox_permission_profile(plan);
    if spec.manifest.permission_profile != expected_profile {
        return Err(DriverError::InvalidContainment(format!(
            "driver manifest permission profile must be {expected_profile}"
        )));
    }
    let launcher = PathBuf::from(&plan.command.program).canonicalize()?;
    match plan.compiled.backend {
        SandboxBackendKind::MacOsSeatbelt => {
            let expected = PathBuf::from("/usr/bin/sandbox-exec").canonicalize()?;
            if launcher != expected {
                return Err(DriverError::InvalidContainment(
                    "macOS sandbox plan does not use the pinned Seatbelt launcher".into(),
                ));
            }
        }
        backend => {
            return Err(DriverError::InvalidContainment(format!(
                "driver sandbox backend {backend:?} is not supported"
            )));
        }
    }
    if plan.command.args.len() > 512
        || plan.command.args.iter().any(|argument| {
            argument.is_empty()
                || argument.len() > MAX_SANDBOX_ARGUMENT_BYTES
                || argument.contains('\0')
        })
    {
        return Err(DriverError::InvalidContainment(
            "sandbox launcher arguments exceed their bound".into(),
        ));
    }
    let sandbox_cwd = plan
        .command
        .cwd
        .as_deref()
        .ok_or_else(|| {
            DriverError::InvalidContainment(
                "sandbox launcher requires an explicit working directory".into(),
            )
        })?
        .canonicalize()?;
    Ok(ResolvedDriverLaunch {
        executable: launcher,
        arguments: plan.command.args.iter().map(OsString::from).collect(),
        working_directory: sandbox_cwd,
        environment: plan
            .command
            .env
            .iter()
            .map(|(name, value)| (name.clone(), OsString::from(value)))
            .collect(),
    })
}

fn audited_command(
    spec: &DriverSpec,
    executable: &Path,
    working_directory: &Path,
) -> Result<TerminalCommand, DriverError> {
    let program = executable
        .to_str()
        .ok_or_else(|| {
            DriverError::InvalidContainment("driver executable path is not UTF-8".into())
        })?
        .to_owned();
    let args = spec
        .arguments
        .iter()
        .map(|argument| {
            argument.to_str().map(str::to_owned).ok_or_else(|| {
                DriverError::InvalidContainment("driver argument is not UTF-8".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let environment = spec
        .environment
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.clone(), value.to_owned()))
                .ok_or_else(|| {
                    DriverError::InvalidContainment(format!(
                        "driver environment variable {name} is not UTF-8"
                    ))
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(TerminalCommand {
        program,
        args,
        cwd: Some(working_directory.to_path_buf()),
        env: environment,
    })
}

pub(crate) fn sandbox_permission_profile(plan: &SandboxLaunchPlan) -> String {
    let backend = match plan.compiled.backend {
        SandboxBackendKind::MacOsSeatbelt => "macos-seatbelt",
        SandboxBackendKind::LimaVm => "lima-vm",
        SandboxBackendKind::LinuxBubblewrap => "linux-bubblewrap",
        SandboxBackendKind::WindowsRestrictedToken => "windows-restricted-token",
        SandboxBackendKind::TestOnlyUnenforced => "test-only-unenforced",
    };
    format!("{backend}:{}", plan.compiled.profile_digest)
}

impl Drop for DriverProcess {
    fn drop(&mut self) {
        let _ = self.stop(Duration::from_millis(100));
    }
}

fn spawn_protocol_reader(
    shared: Arc<DriverShared>,
    output: impl Read + Send + 'static,
    framing: DriverFraming,
    max_frame_bytes: usize,
    max_pending_output_bytes: usize,
    events: Sender<QueuedDriverEvent>,
) -> Result<(), DriverError> {
    thread::Builder::new()
        .name("hyper-driver-protocol".into())
        .spawn(move || {
            let mut reader = BufReader::new(output);
            let mut sequence = 0;
            loop {
                match framing.read_sized(&mut reader, max_frame_bytes) {
                    Ok(Some(frame)) => {
                        if let Err(error) = reserve_pending_output(
                            &shared,
                            frame.payload_bytes,
                            max_pending_output_bytes,
                        ) {
                            let _ = send_driver_event(
                                &events,
                                DriverEvent::ProtocolError {
                                    message: error.to_string(),
                                },
                            );
                            finish_after_output_closed(&shared, &events, true);
                            return;
                        }
                        sequence += 1;
                        if events
                            .send(QueuedDriverEvent {
                                event: DriverEvent::Message {
                                    sequence,
                                    payload: frame.payload,
                                },
                                output_bytes: frame.payload_bytes,
                            })
                            .is_err()
                        {
                            shared
                                .pending_output_bytes
                                .fetch_sub(frame.payload_bytes, Ordering::AcqRel);
                            return;
                        }
                    }
                    Ok(None) => {
                        finish_after_output_closed(&shared, &events, false);
                        return;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = send_driver_event(&events, DriverEvent::ProtocolError { message });
                        finish_after_output_closed(&shared, &events, true);
                        return;
                    }
                }
            }
        })?;
    Ok(())
}

fn reserve_pending_output(
    shared: &DriverShared,
    payload_bytes: usize,
    maximum: usize,
) -> Result<(), DriverError> {
    match shared
        .pending_output_bytes
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(payload_bytes)
                .filter(|pending| *pending <= maximum)
        }) {
        Ok(_) => Ok(()),
        Err(current) => Err(DriverError::PendingOutputBudgetExceeded {
            pending: current.saturating_add(payload_bytes),
            maximum,
        }),
    }
}

fn send_driver_event(
    events: &Sender<QueuedDriverEvent>,
    event: DriverEvent,
) -> Result<(), crossbeam_channel::SendError<QueuedDriverEvent>> {
    events.send(QueuedDriverEvent {
        event,
        output_bytes: 0,
    })
}

fn spawn_stderr_reader(
    shared: Arc<DriverShared>,
    mut stderr: impl Read + Send + 'static,
) -> Result<(), DriverError> {
    thread::Builder::new()
        .name("hyper-driver-stderr".into())
        .spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(read) = stderr.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                if let Ok(mut tail) = shared.stderr.lock() {
                    for byte in &buffer[..read] {
                        if tail.len() == STDERR_TAIL_BYTES {
                            tail.pop_front();
                        }
                        tail.push_back(*byte);
                    }
                }
            }
        })?;
    Ok(())
}

fn finish_after_output_closed(
    shared: &Arc<DriverShared>,
    events: &Sender<QueuedDriverEvent>,
    protocol_failed: bool,
) {
    let mut status = wait_for_exit(shared, EXIT_AFTER_STDOUT_GRACE)
        .ok()
        .flatten();
    if status.is_none() {
        status = terminate_process(shared, Duration::from_millis(100))
            .ok()
            .flatten();
    }
    let previous = shared
        .state
        .lock()
        .map(|state| *state)
        .unwrap_or(DriverState::Failed);
    let uncertain = shared.effect_in_flight.swap(false, Ordering::AcqRel);
    let final_state = if previous.is_terminal() {
        previous
    } else if uncertain {
        DriverState::UnknownExecution
    } else if protocol_failed {
        DriverState::Failed
    } else if previous == DriverState::Closing || status.is_some_and(|value| value.success()) {
        DriverState::Closed
    } else {
        DriverState::Failed
    };
    if let Ok(mut state) = shared.state.lock() {
        *state = final_state;
    }
    if !shared.exit_reported.swap(true, Ordering::AcqRel) {
        let _ = send_driver_event(
            events,
            DriverEvent::Exited {
                code: status.and_then(|value| value.code()),
                state: final_state,
            },
        );
    }
}

fn terminate_process(
    shared: &Arc<DriverShared>,
    grace: Duration,
) -> Result<Option<ExitStatus>, DriverError> {
    let mut status = try_wait(shared)?;
    signal_process_group(shared, false)?;
    let deadline = Instant::now() + grace;
    loop {
        if status.is_none() {
            status = try_wait(shared)?;
        }
        if !process_group_exists(shared)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    signal_process_group(shared, true)?;
    let force_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if status.is_none() {
            status = try_wait(shared)?;
        }
        if !process_group_exists(shared)? {
            return Ok(status);
        }
        if Instant::now() >= force_deadline {
            return Err(DriverError::ProcessGroupDidNotExit {
                process_group_id: shared.process_group_id,
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn try_wait(shared: &Arc<DriverShared>) -> Result<Option<ExitStatus>, DriverError> {
    Ok(lock(&shared.child)?.try_wait()?)
}

fn wait_for_exit(
    shared: &Arc<DriverShared>,
    timeout: Duration,
) -> Result<Option<ExitStatus>, DriverError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = try_wait(shared)? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn signal_process_group(shared: &Arc<DriverShared>, force: bool) -> Result<(), DriverError> {
    let pid = shared.process_group_id as i32;
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: the child is launched as the leader of its own process group and
    // `pid` was returned by `std::process::Child`. No borrowed memory crosses FFI.
    let result = unsafe { libc::killpg(pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.into())
    }
}

#[cfg(unix)]
fn process_group_exists(shared: &Arc<DriverShared>) -> Result<bool, DriverError> {
    // Signal 0 performs existence and permission checking without delivering a
    // signal. The group ID is reserved while any group member still exists.
    let result = unsafe { libc::killpg(shared.process_group_id as i32, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn signal_process_group(shared: &Arc<DriverShared>, _force: bool) -> Result<(), DriverError> {
    lock(&shared.child)?.kill()?;
    Ok(())
}

#[cfg(not(unix))]
fn process_group_exists(shared: &Arc<DriverShared>) -> Result<bool, DriverError> {
    Ok(try_wait(shared)?.is_none())
}

fn validate_spec(spec: &DriverSpec) -> Result<(), DriverError> {
    if !spec.executable.is_absolute() || !spec.working_directory.is_absolute() {
        return Err(DriverError::InvalidSpec(
            "executable and working directory must be absolute".into(),
        ));
    }
    if spec.max_frame_bytes == 0 || spec.max_frame_bytes > DEFAULT_MAX_DRIVER_FRAME_BYTES {
        return Err(DriverError::InvalidSpec(format!(
            "max frame must be between 1 and {DEFAULT_MAX_DRIVER_FRAME_BYTES} bytes"
        )));
    }
    if spec.max_pending_output_bytes == 0
        || spec.max_pending_output_bytes > MAX_PENDING_DRIVER_OUTPUT_BYTES
    {
        return Err(DriverError::InvalidSpec(format!(
            "pending output budget must be between 1 and {MAX_PENDING_DRIVER_OUTPUT_BYTES} bytes"
        )));
    }
    if spec.arguments.len() > 128
        || spec
            .arguments
            .iter()
            .any(|value| value.to_string_lossy().len() > 32 * 1024)
    {
        return Err(DriverError::InvalidSpec(
            "driver arguments exceed their configured bound".into(),
        ));
    }
    if spec.environment.len() > 64 {
        return Err(DriverError::InvalidSpec(
            "driver environment exceeds 64 entries".into(),
        ));
    }
    for (name, value) in &spec.environment {
        if !valid_environment_name(name)
            || name.starts_with("DYLD_")
            || name.starts_with("LD_")
            || value.to_string_lossy().len() > 32 * 1024
        {
            return Err(DriverError::InvalidSpec(format!(
                "driver environment entry {name} is not allowed"
            )));
        }
    }
    if spec.manifest.implementation_version.is_empty()
        || spec.manifest.protocol_version.is_empty()
        || spec.manifest.permission_profile.is_empty()
        || !is_sha256(&spec.manifest.executable_sha256)
    {
        return Err(DriverError::InvalidSpec(
            "driver manifest is incomplete".into(),
        ));
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn sha256_file(path: &Path) -> Result<String, DriverError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, DriverError> {
    mutex.lock().map_err(|_| DriverError::SupervisorPoisoned)
}

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("driver I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("driver JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid driver frame: {0}")]
    InvalidFrame(String),
    #[error("driver frame is {size} bytes; maximum is {maximum}")]
    FrameTooLarge { size: usize, maximum: usize },
    #[error("driver pending output is {pending} bytes; maximum is {maximum}")]
    PendingOutputBudgetExceeded { pending: usize, maximum: usize },
    #[error("invalid driver specification: {0}")]
    InvalidSpec(String),
    #[error("executable digest mismatch: expected {expected}, got {actual}")]
    ExecutableDigestMismatch { expected: String, actual: String },
    #[error("invalid driver containment: {0}")]
    InvalidContainment(String),
    #[error("sandbox action digest mismatch: expected {expected}, got {actual}")]
    SandboxActionDigestMismatch { expected: String, actual: String },
    #[error("spawned driver did not expose {0}")]
    MissingPipe(&'static str),
    #[error("driver input is closed")]
    InputClosed,
    #[error("driver event stream timed out")]
    Timeout,
    #[error("driver effect timed out and was terminated in state {state:?}")]
    EffectTimedOut { state: DriverState },
    #[error("driver event stream closed")]
    EventStreamClosed,
    #[error("driver expected {expected}, but was {actual:?}")]
    IllegalState {
        expected: &'static str,
        actual: DriverState,
    },
    #[error("driver supervisor lock was poisoned")]
    SupervisorPoisoned,
    #[error("driver process group {process_group_id} did not exit after SIGKILL")]
    ProcessGroupDidNotExit { process_group_id: u32 },
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsString;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn supervised_json_line_process_is_ordered_and_reaped() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let process = DriverProcess::spawn(DriverSpec {
            manifest: manifest(&shell, DriverKind::ClaudeStreamJson),
            executable: shell,
            arguments: vec![
                OsString::from("-c"),
                OsString::from("printf '{\"kind\":\"ready\"}\\n'"),
            ],
            working_directory: temp.path().canonicalize().unwrap(),
            environment: BTreeMap::new(),
            sandbox: None,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })
        .unwrap();
        assert_eq!(
            process.recv_timeout(Duration::from_secs(1)).unwrap(),
            DriverEvent::Message {
                sequence: 1,
                payload: serde_json::json!({"kind": "ready"})
            }
        );
        assert!(matches!(
            process.recv_timeout(Duration::from_secs(1)).unwrap(),
            DriverEvent::Exited {
                state: DriverState::Closed,
                ..
            }
        ));
    }

    #[test]
    fn executable_digest_is_checked_before_spawn() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let mut manifest = manifest(&shell, DriverKind::DenoLsp);
        manifest.executable_sha256 = "0".repeat(64);
        let result = DriverProcess::spawn(DriverSpec {
            manifest,
            executable: shell,
            arguments: vec![],
            working_directory: temp.path().canonicalize().unwrap(),
            environment: BTreeMap::new(),
            sandbox: None,
            framing: DriverFraming::ContentLength,
            max_frame_bytes: 1024,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        });
        assert!(matches!(
            result,
            Err(DriverError::ExecutableDigestMismatch { .. })
        ));
    }

    #[test]
    fn stopping_during_an_effect_is_never_reported_as_safe_to_replay() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let process = DriverProcess::spawn(DriverSpec {
            manifest: manifest(&shell, DriverKind::AcpAgent),
            executable: shell,
            arguments: vec![OsString::from("-c"), OsString::from("sleep 30")],
            working_directory: temp.path().canonicalize().unwrap(),
            environment: BTreeMap::new(),
            sandbox: None,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })
        .unwrap();
        process.mark_ready().unwrap();
        process.begin_effect().unwrap();
        assert_eq!(
            process.stop(Duration::from_millis(50)).unwrap(),
            DriverState::UnknownExecution
        );
    }

    #[test]
    fn timing_out_an_effect_kills_it_and_forbids_automatic_replay() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let process = DriverProcess::spawn(DriverSpec {
            manifest: manifest(&shell, DriverKind::DenoGenUi),
            executable: shell,
            arguments: vec![OsString::from("-c"), OsString::from("sleep 30")],
            working_directory: temp.path().canonicalize().unwrap(),
            environment: BTreeMap::new(),
            sandbox: None,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })
        .unwrap();
        process.mark_ready().unwrap();
        process.begin_effect().unwrap();

        assert!(matches!(
            process.recv_timeout(Duration::from_millis(20)),
            Err(DriverError::EffectTimedOut {
                state: DriverState::UnknownExecution
            })
        ));
        assert_eq!(process.state().unwrap(), DriverState::UnknownExecution);
    }

    #[test]
    fn pending_output_budget_fails_closed_during_an_effect() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let payload = "x".repeat(700);
        let script = format!(
            "read ignored; printf '%s\\n%s\\n' '{{\"data\":\"{payload}\"}}' '{{\"data\":\"{payload}\"}}'"
        );
        let process = DriverProcess::spawn(DriverSpec {
            manifest: manifest(&shell, DriverKind::DenoGenUi),
            executable: shell,
            arguments: vec![OsString::from("-c"), OsString::from(script)],
            working_directory: temp.path().canonicalize().unwrap(),
            environment: BTreeMap::new(),
            sandbox: None,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
            max_pending_output_bytes: 1024,
        })
        .unwrap();
        process.mark_ready().unwrap();
        process.begin_effect().unwrap();
        process
            .send_json(&serde_json::json!({"run": true}))
            .unwrap();
        thread::sleep(Duration::from_millis(50));

        assert!(matches!(
            process.recv_timeout(Duration::from_secs(1)).unwrap(),
            DriverEvent::Message { sequence: 1, .. }
        ));
        let DriverEvent::ProtocolError { message } =
            process.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("output overflow did not produce a protocol error")
        };
        assert!(message.contains("pending output"));
        assert!(matches!(
            process.recv_timeout(Duration::from_secs(1)).unwrap(),
            DriverEvent::Exited {
                state: DriverState::UnknownExecution,
                ..
            }
        ));
    }

    #[test]
    fn secondary_inbox_is_bounded_by_bytes_and_releases_consumed_events() {
        let event = DriverEvent::Message {
            sequence: 1,
            payload: serde_json::json!({"data": "x".repeat(700)}),
        };
        let mut inbox = BoundedDriverInbox::new(8, 1024);
        inbox.push_back(event.clone()).unwrap();
        assert!(inbox.push_back(event.clone()).is_err());
        assert_eq!(inbox.pop_front(), Some(event.clone()));
        assert!(inbox.push_back(event).is_ok());
    }

    #[test]
    fn stop_reaps_descendants_that_ignore_sigterm() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let child_pid_path = temp.path().join("child.pid");
        let process = DriverProcess::spawn(DriverSpec {
            manifest: manifest(&shell, DriverKind::AcpAgent),
            executable: shell,
            arguments: vec![
                OsString::from("-c"),
                OsString::from(
                    "trap '' TERM; (trap '' TERM; sleep 30) & printf '%s' \"$!\" > \"$CHILD_PID_PATH\"; wait",
                ),
            ],
            working_directory: temp.path().canonicalize().unwrap(),
            environment: BTreeMap::from([(
                "CHILD_PID_PATH".into(),
                child_pid_path.clone().into_os_string(),
            )]),
            sandbox: None,
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })
        .unwrap();
        let process_group_id = process.shared.process_group_id;
        let deadline = Instant::now() + Duration::from_secs(1);
        while !child_pid_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let child_pid: i32 = std::fs::read_to_string(&child_pid_path)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(child_pid, 0) }, 0);

        assert_eq!(
            process.stop(Duration::from_millis(50)).unwrap(),
            DriverState::Closed
        );
        assert!(!process_group_exists(&process.shared).unwrap());
        assert_ne!(process_group_id, std::process::id());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_plan_is_bound_to_the_exact_audited_driver_command() {
        let temp = TempDir::new().unwrap();
        let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let working_directory = temp.path().canonicalize().unwrap();
        let original_arguments = vec![OsString::from("-c"), OsString::from("printf original")];
        let driver_id = Uuid::new_v4();
        let plan = crate::deno_containment::compile_deno_task_sandbox(
            driver_id,
            &shell,
            &original_arguments,
            &working_directory,
            &BTreeMap::new(),
            Vec::new(),
            [working_directory.clone()],
        )
        .unwrap();
        let permission_profile = sandbox_permission_profile(&plan);
        let result = DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id,
                permission_profile,
                ..manifest(&shell, DriverKind::DenoGenUi)
            },
            executable: shell,
            arguments: vec![OsString::from("-c"), OsString::from("printf changed")],
            working_directory,
            environment: BTreeMap::new(),
            sandbox: Some(plan),
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        });
        assert!(matches!(
            result,
            Err(DriverError::SandboxActionDigestMismatch { .. })
        ));
    }

    fn manifest(executable: &Path, kind: DriverKind) -> DriverManifest {
        DriverManifest {
            driver_id: Uuid::new_v4(),
            kind,
            implementation_version: "test".into(),
            protocol_version: "test-v1".into(),
            capabilities: vec![],
            transport: "stdio".into(),
            executable_sha256: sha256_file(executable).unwrap(),
            permission_profile: "test-no-authority".into(),
        }
    }
}
