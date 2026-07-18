use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{DEFAULT_MAX_DRIVER_FRAME_BYTES, DriverFraming};

const DRIVER_EVENT_CAPACITY: usize = 256;
const STDERR_TAIL_BYTES: usize = 64 * 1024;
const EXIT_AFTER_STDOUT_GRACE: Duration = Duration::from_millis(200);

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
    pub framing: DriverFraming,
    pub max_frame_bytes: usize,
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

pub struct DriverProcess {
    manifest: DriverManifest,
    framing: DriverFraming,
    max_frame_bytes: usize,
    input: Mutex<Option<ChildStdin>>,
    shared: Arc<DriverShared>,
    events: Receiver<DriverEvent>,
}

struct DriverShared {
    child: Mutex<Child>,
    state: Mutex<DriverState>,
    stderr: Mutex<VecDeque<u8>>,
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
        let mut command = Command::new(executable);
        command
            .args(&spec.arguments)
            .current_dir(working_directory)
            .env_clear()
            .envs(&spec.environment)
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
        let shared = Arc::new(DriverShared {
            child: Mutex::new(child),
            state: Mutex::new(DriverState::Starting),
            stderr: Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)),
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
        self.events
            .recv_timeout(timeout)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => DriverError::Timeout,
                RecvTimeoutError::Disconnected => DriverError::EventStreamClosed,
            })
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
        let uncertain = self.shared.effect_in_flight.swap(false, Ordering::AcqRel);
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
    events: Sender<DriverEvent>,
) -> Result<(), DriverError> {
    thread::Builder::new()
        .name("hyper-driver-protocol".into())
        .spawn(move || {
            let mut reader = BufReader::new(output);
            let mut sequence = 0;
            loop {
                match framing.read(&mut reader, max_frame_bytes) {
                    Ok(Some(payload)) => {
                        sequence += 1;
                        if events
                            .send(DriverEvent::Message { sequence, payload })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(None) => {
                        finish_after_output_closed(&shared, &events, false);
                        return;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = events.send(DriverEvent::ProtocolError { message });
                        finish_after_output_closed(&shared, &events, true);
                        return;
                    }
                }
            }
        })?;
    Ok(())
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
    events: &Sender<DriverEvent>,
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
        let _ = events.send(DriverEvent::Exited {
            code: status.and_then(|value| value.code()),
            state: final_state,
        });
    }
}

fn terminate_process(
    shared: &Arc<DriverShared>,
    grace: Duration,
) -> Result<Option<ExitStatus>, DriverError> {
    if let Some(status) = try_wait(shared)? {
        return Ok(Some(status));
    }
    signal_process_group(shared, false)?;
    if let Some(status) = wait_for_exit(shared, grace)? {
        return Ok(Some(status));
    }
    signal_process_group(shared, true)?;
    wait_for_exit(shared, Duration::from_secs(1))
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
    let pid = lock(&shared.child)?.id() as i32;
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

#[cfg(not(unix))]
fn signal_process_group(shared: &Arc<DriverShared>, _force: bool) -> Result<(), DriverError> {
    lock(&shared.child)?.kill()?;
    Ok(())
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
    #[error("invalid driver specification: {0}")]
    InvalidSpec(String),
    #[error("executable digest mismatch: expected {expected}, got {actual}")]
    ExecutableDigestMismatch { expected: String, actual: String },
    #[error("spawned driver did not expose {0}")]
    MissingPipe(&'static str),
    #[error("driver input is closed")]
    InputClosed,
    #[error("driver event stream timed out")]
    Timeout,
    #[error("driver event stream closed")]
    EventStreamClosed,
    #[error("driver expected {expected}, but was {actual:?}")]
    IllegalState {
        expected: &'static str,
        actual: DriverState,
    },
    #[error("driver supervisor lock was poisoned")]
    SupervisorPoisoned,
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
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
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
            framing: DriverFraming::ContentLength,
            max_frame_bytes: 1024,
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
            framing: DriverFraming::JsonLines,
            max_frame_bytes: 1024,
        })
        .unwrap();
        process.mark_ready().unwrap();
        process.begin_effect().unwrap();
        assert_eq!(
            process.stop(Duration::from_millis(50)).unwrap(),
            DriverState::UnknownExecution
        );
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
