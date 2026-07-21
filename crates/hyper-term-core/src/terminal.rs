use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use hyper_term_protocol::{SandboxBackendKind, TerminalCommand, TerminalId, TerminalSize};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use thiserror::Error;

const READER_CHUNK_BYTES: usize = 16 * 1024;
const SUBSCRIBER_QUEUE_CHUNKS: usize = 256;
#[cfg(unix)]
const BURST_COALESCE_WINDOW: Duration = Duration::from_millis(2);
#[cfg(unix)]
const BURST_COALESCE_POLL_MS: i32 = 1;
#[cfg(unix)]
const BURST_COALESCE_MIN_BYTES: usize = 512;

const USER_SHELL_TERM: &str = "xterm-256color";
const USER_SHELL_COLORTERM: &str = "truecolor";
const USER_SHELL_TERM_PROGRAM: &str = "HyperTerm";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UserShellConfig {
    /// An authority-owned override for tests or a future settings service.
    /// This field is deliberately absent from the renderer wire protocol.
    pub shell: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserShellProfile {
    pub program: PathBuf,
    pub cwd: Option<PathBuf>,
    pub login: bool,
}

impl UserShellConfig {
    pub fn resolved_profile(&self) -> Result<UserShellProfile, TerminalError> {
        validate_working_directory(self.cwd.as_deref())?;

        let program = if let Some(program) = &self.shell {
            validate_shell_program(program)?;
            program.clone()
        } else {
            let mut builder = CommandBuilder::new_default_prog();
            apply_user_environment(&mut builder, self, None);
            let program = PathBuf::from(builder.get_shell());
            validate_shell_program(&program)?;
            program
        };

        let login = self.shell.is_none() || !login_arguments(&program).is_empty();
        Ok(UserShellProfile {
            program,
            cwd: self.cwd.clone(),
            login,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalChunk {
    pub terminal_id: TerminalId,
    pub sequence: u64,
    pub bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalExit {
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalEvent {
    Output(TerminalChunk),
    Exited(TerminalExit),
    Fault(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSnapshot {
    pub terminal_id: TerminalId,
    pub base_sequence: u64,
    pub next_sequence: u64,
    pub total_bytes: u64,
    pub tail: Vec<u8>,
    pub exit: Option<TerminalExit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalReplay {
    Chunks(Vec<TerminalChunk>),
    SnapshotRequired(TerminalSnapshot),
}

pub struct TerminalSubscription {
    pub replay: TerminalReplay,
    pub receiver: Receiver<TerminalEvent>,
    pub exit: Option<TerminalExit>,
}

pub struct TerminalConfig {
    pub replay_bytes: usize,
    pub transcript_tail_bytes: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            replay_bytes: 4 * 1024 * 1024,
            transcript_tail_bytes: 512 * 1024,
        }
    }
}

#[derive(Clone)]
pub struct TerminalSessionHandle {
    inner: Arc<TerminalSession>,
}

impl TerminalSessionHandle {
    pub fn id(&self) -> TerminalId {
        self.inner.id
    }

    pub fn process_id(&self) -> Option<u32> {
        self.inner.process_id
    }

    pub fn subscribe(&self, after_sequence: u64) -> TerminalSubscription {
        self.inner.replay.subscribe(after_sequence)
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        self.inner.replay.snapshot()
    }

    pub fn write_input(&self, input_sequence: u64, bytes: &[u8]) -> Result<(), TerminalError> {
        if bytes.len() > READER_CHUNK_BYTES {
            return Err(TerminalError::InputTooLarge(bytes.len()));
        }
        let mut last_sequence = lock(&self.inner.last_input_sequence)?;
        if input_sequence <= *last_sequence {
            return Err(TerminalError::StaleInputSequence {
                current: *last_sequence,
                actual: input_sequence,
            });
        }
        let mut writer = lock(&self.inner.writer)?;
        writer.write_all(bytes)?;
        writer.flush()?;
        *last_sequence = input_sequence;
        Ok(())
    }

    pub fn resize(&self, generation: u64, size: &TerminalSize) -> Result<(), TerminalError> {
        size.validate().map_err(TerminalError::InvalidSize)?;
        let mut current_generation = lock(&self.inner.resize_generation)?;
        if generation <= *current_generation {
            return Err(TerminalError::StaleResizeGeneration {
                current: *current_generation,
                actual: generation,
            });
        }
        lock(&self.inner.master)?.resize(to_pty_size(size))?;
        *current_generation = generation;
        Ok(())
    }

    pub fn close(&self) -> Result<(), TerminalError> {
        if self.snapshot().exit.is_some() {
            return Ok(());
        }
        lock(&self.inner.killer)?.kill()?;
        Ok(())
    }
}

struct TerminalSession {
    id: TerminalId,
    process_id: Option<u32>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    last_input_sequence: Mutex<u64>,
    resize_generation: Mutex<u64>,
    replay: Arc<TerminalReplayBuffer>,
}

#[derive(Default)]
pub struct TerminalSupervisor {
    sessions: Mutex<HashMap<TerminalId, TerminalSessionHandle>>,
}

impl TerminalSupervisor {
    pub fn spawn(
        &self,
        command: &TerminalCommand,
        size: &TerminalSize,
        config: TerminalConfig,
    ) -> Result<TerminalSessionHandle, TerminalError> {
        size.validate().map_err(TerminalError::InvalidSize)?;
        if command.program.trim().is_empty() {
            return Err(TerminalError::EmptyProgram);
        }

        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        if let Some(cwd) = &command.cwd {
            builder.cwd(cwd);
        }
        for (key, value) in &command.env {
            builder.env(key, value);
        }

        self.spawn_builder(builder, size, config)
    }

    pub fn spawn_sandboxed(
        &self,
        plan: &crate::SandboxLaunchPlan,
        size: &TerminalSize,
        config: TerminalConfig,
    ) -> Result<TerminalSessionHandle, TerminalError> {
        if !plan.compiled.enforced
            || plan.compiled.backend == SandboxBackendKind::TestOnlyUnenforced
        {
            return Err(TerminalError::UnenforcedSandboxPlan);
        }
        if !plan.clear_environment {
            return Err(TerminalError::SandboxEnvironmentNotCleared);
        }
        size.validate().map_err(TerminalError::InvalidSize)?;
        if plan.command.program.trim().is_empty() {
            return Err(TerminalError::EmptyProgram);
        }

        let mut builder = CommandBuilder::new(&plan.command.program);
        builder.args(&plan.command.args);
        builder.env_clear();
        if let Some(cwd) = &plan.command.cwd {
            builder.cwd(cwd);
        }
        for (key, value) in &plan.command.env {
            builder.env(key, value);
        }
        self.spawn_builder(builder, size, config)
    }

    pub fn spawn_user_shell(
        &self,
        user_shell: &UserShellConfig,
        size: &TerminalSize,
        config: TerminalConfig,
    ) -> Result<TerminalSessionHandle, TerminalError> {
        let profile = user_shell.resolved_profile()?;
        let mut builder = if user_shell.shell.is_some() {
            let mut builder = CommandBuilder::new(&profile.program);
            builder.args(login_arguments(&profile.program));
            builder
        } else {
            // portable-pty resolves the passwd/$SHELL default and gives it a
            // login argv[0]. With no command and a controlling PTY, zsh and
            // other normal Unix shells enter interactive mode themselves.
            CommandBuilder::new_default_prog()
        };
        if let Some(cwd) = &profile.cwd {
            builder.cwd(cwd);
        }
        apply_user_environment(&mut builder, user_shell, Some(&profile.program));

        self.spawn_builder(builder, size, config)
    }

    fn spawn_builder(
        &self,
        builder: CommandBuilder,
        size: &TerminalSize,
        config: TerminalConfig,
    ) -> Result<TerminalSessionHandle, TerminalError> {
        size.validate().map_err(TerminalError::InvalidSize)?;
        let pair = native_pty_system().openpty(to_pty_size(size))?;

        let mut child = pair.slave.spawn_command(builder)?;
        let process_id = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        #[cfg(unix)]
        let reader_poll_fd = pair.master.as_raw_fd();
        let terminal_id = TerminalId::new();
        let replay = Arc::new(TerminalReplayBuffer::new(
            terminal_id,
            config.replay_bytes,
            config.transcript_tail_bytes,
        ));
        let handle = TerminalSessionHandle {
            inner: Arc::new(TerminalSession {
                id: terminal_id,
                process_id,
                master: Mutex::new(pair.master),
                writer: Mutex::new(writer),
                killer: Mutex::new(killer),
                last_input_sequence: Mutex::new(0),
                resize_generation: Mutex::new(0),
                replay: Arc::clone(&replay),
            }),
        };

        let reader_replay = Arc::clone(&replay);
        let reader_thread = thread::Builder::new()
            .name(format!("terminal-reader-{terminal_id}"))
            .spawn(move || {
                let mut buffer = vec![0_u8; READER_CHUNK_BYTES];
                let mut buffered = 0;
                #[cfg(unix)]
                let mut last_publish: Option<(Instant, usize)> = None;
                loop {
                    match reader.read(&mut buffer[buffered..]) {
                        Ok(0) => {
                            if buffered > 0 {
                                reader_replay.publish_output(&buffer[..buffered]);
                            }
                            break;
                        }
                        Ok(length) => {
                            buffered += length;
                            #[cfg(unix)]
                            let can_coalesce = buffered < buffer.len()
                                && pty_has_pending_output(
                                    reader_poll_fd,
                                    last_publish.is_some_and(|(published_at, published_bytes)| {
                                        published_bytes >= BURST_COALESCE_MIN_BYTES
                                            && published_at.elapsed() <= BURST_COALESCE_WINDOW
                                    }),
                                );
                            #[cfg(not(unix))]
                            let can_coalesce = false;
                            if buffered == buffer.len() || !can_coalesce {
                                #[cfg(unix)]
                                let published_bytes = buffered;
                                reader_replay.publish_output(&buffer[..buffered]);
                                buffered = 0;
                                #[cfg(unix)]
                                {
                                    last_publish = Some((Instant::now(), published_bytes));
                                }
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            if buffered > 0 {
                                reader_replay.publish_output(&buffer[..buffered]);
                            }
                            reader_replay.publish_fault(format!("PTY read failed: {error}"));
                            break;
                        }
                    }
                }
            })?;

        let waiter_replay = Arc::clone(&replay);
        thread::Builder::new()
            .name(format!("terminal-wait-{terminal_id}"))
            .spawn(move || {
                let status = child.wait();
                // The process can report exit before the PTY master has read
                // its final bytes. Join the reader so Exited is a true stream
                // barrier and reconnecting clients never miss the tail.
                if reader_thread.join().is_err() {
                    waiter_replay.publish_fault("PTY reader thread panicked".into());
                }
                match status {
                    Ok(status) => waiter_replay.publish_exit(TerminalExit {
                        exit_code: Some(status.exit_code()),
                        signal: status.signal().map(str::to_owned),
                    }),
                    Err(error) => {
                        waiter_replay.publish_fault(format!("PTY wait failed: {error}"));
                        waiter_replay.publish_exit(TerminalExit {
                            exit_code: None,
                            signal: None,
                        });
                    }
                }
            })?;

        drop(pair.slave);
        lock(&self.sessions)?.insert(terminal_id, handle.clone());
        Ok(handle)
    }

    pub fn get(&self, terminal_id: TerminalId) -> Result<TerminalSessionHandle, TerminalError> {
        lock(&self.sessions)?
            .get(&terminal_id)
            .cloned()
            .ok_or(TerminalError::NotFound(terminal_id))
    }

    pub fn close(&self, terminal_id: TerminalId) -> Result<(), TerminalError> {
        self.get(terminal_id)?.close()
    }
}

fn apply_user_environment(
    builder: &mut CommandBuilder,
    user_shell: &UserShellConfig,
    explicit_program: Option<&Path>,
) {
    for (key, value) in &user_shell.environment {
        builder.env(key, value);
    }
    if let Some(program) = explicit_program {
        builder.env("SHELL", program);
    }
    // These are terminal capabilities, not arbitrary renderer-controlled env.
    // Apply them last so a caller cannot accidentally downgrade the contract.
    builder.env("TERM", USER_SHELL_TERM);
    builder.env("COLORTERM", USER_SHELL_COLORTERM);
    builder.env("TERM_PROGRAM", USER_SHELL_TERM_PROGRAM);
    builder.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    builder.env("HYPER_TERM", "1");
}

fn login_arguments(program: &Path) -> &'static [&'static str] {
    match program.file_name().and_then(|name| name.to_str()) {
        Some("fish") => &["--login"],
        Some("zsh" | "bash" | "sh" | "dash" | "ksh" | "mksh" | "csh" | "tcsh") => &["-l"],
        _ => &[],
    }
}

fn validate_working_directory(cwd: Option<&Path>) -> Result<(), TerminalError> {
    let Some(cwd) = cwd else {
        return Ok(());
    };
    if !cwd.is_absolute() {
        return Err(TerminalError::WorkingDirectoryNotAbsolute(cwd.to_owned()));
    }
    if !cwd.is_dir() {
        return Err(TerminalError::InvalidWorkingDirectory(cwd.to_owned()));
    }
    Ok(())
}

fn validate_shell_program(program: &Path) -> Result<(), TerminalError> {
    if !program.is_absolute() {
        return Err(TerminalError::ShellPathNotAbsolute(program.to_owned()));
    }
    let metadata = std::fs::metadata(program)
        .map_err(|_| TerminalError::ShellNotExecutable(program.to_owned()))?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(TerminalError::ShellNotExecutable(program.to_owned()));
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn pty_has_pending_output(
    file_descriptor: Option<portable_pty::unix::RawFd>,
    rapid_follow_up: bool,
) -> bool {
    let Some(file_descriptor) = file_descriptor else {
        return false;
    };
    let mut descriptor = libc::pollfd {
        fd: file_descriptor,
        events: libc::POLLIN,
        revents: 0,
    };
    // The first isolated output is published immediately. Once reads arrive
    // in a rapid stream, allow one millisecond for the producer to refill the
    // PTY so sustained output reaches the ordered channel in bounded chunks.
    // This preserves first-byte latency while avoiding thousands of tiny
    // publications when the reader temporarily outruns the child process.
    let timeout_ms = if rapid_follow_up {
        BURST_COALESCE_POLL_MS
    } else {
        0
    };
    unsafe {
        libc::poll(&mut descriptor, 1, timeout_ms) > 0 && descriptor.revents & libc::POLLIN != 0
    }
}

struct TerminalReplayBuffer {
    terminal_id: TerminalId,
    replay_bytes: usize,
    transcript_tail_bytes: usize,
    state: Mutex<ReplayState>,
}

struct ReplayState {
    next_sequence: u64,
    total_bytes: u64,
    chunks: VecDeque<TerminalChunk>,
    replay_size: usize,
    transcript_tail: VecDeque<u8>,
    subscribers: Vec<Sender<TerminalEvent>>,
    exit: Option<TerminalExit>,
}

impl TerminalReplayBuffer {
    fn new(terminal_id: TerminalId, replay_bytes: usize, transcript_tail_bytes: usize) -> Self {
        Self {
            terminal_id,
            replay_bytes: replay_bytes.max(1),
            transcript_tail_bytes: transcript_tail_bytes.max(1),
            state: Mutex::new(ReplayState {
                next_sequence: 1,
                total_bytes: 0,
                chunks: VecDeque::new(),
                replay_size: 0,
                transcript_tail: VecDeque::new(),
                subscribers: Vec::new(),
                exit: None,
            }),
        }
    }

    fn publish_output(&self, bytes: &[u8]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let chunk = TerminalChunk {
            terminal_id: self.terminal_id,
            sequence: state.next_sequence,
            bytes: Arc::from(bytes),
        };
        state.next_sequence += 1;
        state.total_bytes = state.total_bytes.saturating_add(bytes.len() as u64);
        state.replay_size += chunk.bytes.len();
        state.chunks.push_back(chunk.clone());
        while state.replay_size > self.replay_bytes && state.chunks.len() > 1 {
            if let Some(removed) = state.chunks.pop_front() {
                state.replay_size = state.replay_size.saturating_sub(removed.bytes.len());
            }
        }
        append_transcript_tail(
            &mut state.transcript_tail,
            bytes,
            self.transcript_tail_bytes,
        );
        publish(&mut state.subscribers, TerminalEvent::Output(chunk));
    }

    fn publish_exit(&self, exit: TerminalExit) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.exit.is_some() {
            return;
        }
        state.exit = Some(exit.clone());
        publish(&mut state.subscribers, TerminalEvent::Exited(exit));
    }

    fn publish_fault(&self, message: String) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        publish(&mut state.subscribers, TerminalEvent::Fault(message));
    }

    fn subscribe(&self, after_sequence: u64) -> TerminalSubscription {
        let (sender, receiver) = bounded(SUBSCRIBER_QUEUE_CHUNKS);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let earliest_sequence = state
            .chunks
            .front()
            .map_or(state.next_sequence, |chunk| chunk.sequence);
        let replay = if after_sequence < earliest_sequence.saturating_sub(1) {
            TerminalReplay::SnapshotRequired(snapshot_from_state(self.terminal_id, &state))
        } else {
            TerminalReplay::Chunks(
                state
                    .chunks
                    .iter()
                    .filter(|chunk| chunk.sequence > after_sequence)
                    .cloned()
                    .collect(),
            )
        };
        let exit = state.exit.clone();
        if exit.is_none() {
            state.subscribers.push(sender);
        }
        TerminalSubscription {
            replay,
            receiver,
            exit,
        }
    }

    fn snapshot(&self) -> TerminalSnapshot {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        snapshot_from_state(self.terminal_id, &state)
    }
}

fn append_transcript_tail(tail: &mut VecDeque<u8>, bytes: &[u8], limit: usize) {
    if bytes.len() >= limit {
        tail.clear();
        tail.extend(&bytes[bytes.len() - limit..]);
        return;
    }
    let overflow = tail.len().saturating_add(bytes.len()).saturating_sub(limit);
    if overflow > 0 {
        tail.drain(..overflow);
    }
    tail.extend(bytes);
}

fn snapshot_from_state(terminal_id: TerminalId, state: &ReplayState) -> TerminalSnapshot {
    TerminalSnapshot {
        terminal_id,
        base_sequence: state
            .chunks
            .front()
            .map_or(state.next_sequence, |chunk| chunk.sequence),
        next_sequence: state.next_sequence,
        total_bytes: state.total_bytes,
        tail: state.transcript_tail.iter().copied().collect(),
        exit: state.exit.clone(),
    }
}

fn publish(subscribers: &mut Vec<Sender<TerminalEvent>>, event: TerminalEvent) {
    subscribers.retain(|sender| match sender.try_send(event.clone()) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    });
}

fn to_pty_size(size: &TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, TerminalError> {
    mutex.lock().map_err(|_| TerminalError::LockPoisoned)
}

#[derive(Debug, Error)]
pub enum TerminalError {
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("PTY error: {0}")]
    Pty(#[from] anyhow::Error),
    #[error("terminal command program is empty")]
    EmptyProgram,
    #[error("sandbox launch plan is not enforced by an operating-system backend")]
    UnenforcedSandboxPlan,
    #[error("sandbox launch plan must clear the inherited environment")]
    SandboxEnvironmentNotCleared,
    #[error("user shell path must be absolute: {0}")]
    ShellPathNotAbsolute(PathBuf),
    #[error("user shell is missing, not a file, or not executable: {0}")]
    ShellNotExecutable(PathBuf),
    #[error("terminal working directory must be absolute: {0}")]
    WorkingDirectoryNotAbsolute(PathBuf),
    #[error("terminal working directory does not exist or is not a directory: {0}")]
    InvalidWorkingDirectory(PathBuf),
    #[error("invalid terminal size: {0}")]
    InvalidSize(&'static str),
    #[error("terminal {0} does not exist")]
    NotFound(TerminalId),
    #[error("terminal input exceeds {READER_CHUNK_BYTES} bytes: {0}")]
    InputTooLarge(usize),
    #[error("stale terminal input sequence {actual}; current is {current}")]
    StaleInputSequence { current: u64, actual: u64 },
    #[error("stale resize generation {actual}; current is {current}")]
    StaleResizeGeneration { current: u64, actual: u64 },
    #[error("terminal state lock is poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use super::*;

    fn wait_for_tail(session: &TerminalSessionHandle, marker: &[u8], timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = session.snapshot();
            if snapshot
                .tail
                .windows(marker.len())
                .any(|window| window == marker)
            {
                return snapshot.tail;
            }
            assert!(
                Instant::now() < deadline,
                "terminal marker did not arrive; tail={:?}",
                String::from_utf8_lossy(&snapshot.tail)
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn shell(script: &str) -> TerminalCommand {
        TerminalCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn sandboxed_spawn_rejects_an_unenforced_test_backend() {
        use crate::SandboxLauncher;
        use hyper_term_protocol::{
            Actor, OperationId, SandboxEnforcement, SandboxEnvironmentPolicy,
            SandboxFileSystemPolicy, SandboxLifetime, SandboxNetworkPolicy, SandboxProcessPolicy,
            SandboxResourceLimits,
        };

        let request = crate::SandboxCompileRequest {
            operation_id: OperationId::new(),
            operation_revision: 4,
            actor: Actor::System,
            command: TerminalCommand {
                program: "/usr/bin/true".into(),
                args: Vec::new(),
                cwd: Some("/tmp".into()),
                env: BTreeMap::new(),
            },
            profile: hyper_term_protocol::SandboxProfile {
                enforcement: SandboxEnforcement::Native,
                filesystem: SandboxFileSystemPolicy::default(),
                network: SandboxNetworkPolicy::Offline,
                environment: SandboxEnvironmentPolicy::default(),
                process: SandboxProcessPolicy::default(),
                resources: SandboxResourceLimits::default(),
                lifetime: SandboxLifetime::OneOperation,
            },
        };
        let plan = crate::TestOnlyUnenforcedSandboxLauncher
            .compile(&request)
            .unwrap();
        let result = TerminalSupervisor::default().spawn_sandboxed(
            &plan,
            &TerminalSize::default(),
            TerminalConfig::default(),
        );
        assert!(matches!(result, Err(TerminalError::UnenforcedSandboxPlan)));
    }

    #[test]
    fn real_pty_output_is_ordered_and_replayable() {
        let supervisor = TerminalSupervisor::default();
        let session = supervisor
            .spawn(
                &shell("printf alpha; printf beta"),
                &TerminalSize::default(),
                TerminalConfig::default(),
            )
            .expect("spawn PTY");
        let subscription = session.subscribe(0);
        let mut chunks = match subscription.replay {
            TerminalReplay::Chunks(chunks) => chunks,
            TerminalReplay::SnapshotRequired(_) => panic!("fresh cursor should replay chunks"),
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match subscription
                .receiver
                .recv_timeout(Duration::from_millis(50))
            {
                Ok(TerminalEvent::Output(chunk)) => chunks.push(chunk),
                Ok(TerminalEvent::Exited(_)) => break,
                Ok(TerminalEvent::Fault(message)) => panic!("terminal fault: {message}"),
                Err(crossbeam_channel::RecvTimeoutError::Timeout)
                    if session.snapshot().exit.is_some() =>
                {
                    break;
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("subscription failed: {error}"),
            }
        }
        chunks.sort_by_key(|chunk| chunk.sequence);
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].sequence + 1, pair[1].sequence);
        }
        let bytes = chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect::<Vec<_>>();
        assert!(String::from_utf8_lossy(&bytes).contains("alphabeta"));
    }

    #[test]
    fn exit_event_is_a_barrier_after_the_final_pty_bytes() {
        const OUTPUT_BYTES: usize = 1024 * 1024;
        let supervisor = TerminalSupervisor::default();
        let session = supervisor
            .spawn(
                &TerminalCommand {
                    program: "/usr/bin/head".into(),
                    args: vec!["-c".into(), OUTPUT_BYTES.to_string(), "/dev/zero".into()],
                    cwd: None,
                    env: BTreeMap::new(),
                },
                &TerminalSize::default(),
                TerminalConfig::default(),
            )
            .expect("spawn burst PTY");
        let subscription = session.subscribe(0);
        let mut observed_bytes = match subscription.replay {
            TerminalReplay::Chunks(chunks) => {
                chunks.iter().map(|chunk| chunk.bytes.len()).sum::<usize>()
            }
            TerminalReplay::SnapshotRequired(snapshot) => snapshot.total_bytes as usize,
        };
        if subscription.exit.is_none() {
            loop {
                match subscription
                    .receiver
                    .recv_timeout(Duration::from_secs(3))
                    .expect("terminal event")
                {
                    TerminalEvent::Output(chunk) => observed_bytes += chunk.bytes.len(),
                    TerminalEvent::Exited(_) => break,
                    TerminalEvent::Fault(message) => panic!("terminal fault: {message}"),
                }
            }
        }
        assert_eq!(observed_bytes, OUTPUT_BYTES);
        assert_eq!(session.snapshot().total_bytes as usize, OUTPUT_BYTES);
    }

    #[test]
    fn dropping_a_client_subscription_does_not_kill_the_pty() {
        let supervisor = TerminalSupervisor::default();
        let session = supervisor
            .spawn(
                &shell("sleep 0.05; printf survivor"),
                &TerminalSize::default(),
                TerminalConfig::default(),
            )
            .expect("spawn PTY");
        drop(session.subscribe(0));

        let deadline = Instant::now() + Duration::from_secs(3);
        while session.snapshot().exit.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let snapshot = session.snapshot();
        assert!(snapshot.exit.is_some());
        assert!(String::from_utf8_lossy(&snapshot.tail).contains("survivor"));
    }

    #[test]
    fn stale_input_and_resize_generations_are_rejected() {
        let supervisor = TerminalSupervisor::default();
        let session = supervisor
            .spawn(
                &shell("cat"),
                &TerminalSize::default(),
                TerminalConfig::default(),
            )
            .expect("spawn PTY");
        session.write_input(1, b"one\n").unwrap();
        assert!(matches!(
            session.write_input(1, b"duplicate\n"),
            Err(TerminalError::StaleInputSequence { .. })
        ));
        session.resize(1, &TerminalSize::default()).unwrap();
        assert!(matches!(
            session.resize(1, &TerminalSize::default()),
            Err(TerminalError::StaleResizeGeneration { .. })
        ));
        session.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_zsh_is_login_interactive_truecolor_and_utf8_capable() {
        let zsh = PathBuf::from("/bin/zsh");
        if !zsh.exists() {
            return;
        }
        let home = tempfile::tempdir().expect("temporary shell home");
        let mut environment = BTreeMap::new();
        environment.insert("HOME".into(), home.path().display().to_string());
        environment.insert("ZDOTDIR".into(), home.path().display().to_string());
        environment.insert("TERM".into(), "dumb".into());
        let config = UserShellConfig {
            shell: Some(zsh.clone()),
            cwd: Some(home.path().to_owned()),
            environment,
        };
        let profile = config.resolved_profile().expect("resolve zsh");
        assert_eq!(profile.program, zsh);
        assert!(profile.login);

        let supervisor = TerminalSupervisor::default();
        let session = supervisor
            .spawn_user_shell(&config, &TerminalSize::default(), TerminalConfig::default())
            .expect("spawn zsh");
        session
            .write_input(
                1,
                b"if [[ -o login && -o interactive ]]; then print -r -- '__HYPER_ZSH__:login:interactive:'$TERM':'$COLORTERM':'$TERM_PROGRAM':\xE4\xB8\xAD\xE6\x96\x87'; else print -r -- '__HYPER_ZSH__:wrong-mode'; fi; exit\n",
            )
            .expect("write zsh probe");
        let expected = b"__HYPER_ZSH__:login:interactive:xterm-256color:truecolor:HyperTerm:\xE4\xB8\xAD\xE6\x96\x87";
        let output = wait_for_tail(&session, expected, Duration::from_secs(5));
        assert!(
            output
                .windows(expected.len())
                .any(|window| window == expected),
            "unexpected zsh probe output: {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[cfg(unix)]
    #[test]
    fn control_c_interrupts_a_foreground_job_without_killing_zsh() {
        let zsh = PathBuf::from("/bin/zsh");
        if !zsh.exists() {
            return;
        }
        let home = tempfile::tempdir().expect("temporary shell home");
        let config = UserShellConfig {
            shell: Some(zsh),
            cwd: Some(home.path().to_owned()),
            environment: BTreeMap::from([
                ("HOME".into(), home.path().display().to_string()),
                ("ZDOTDIR".into(), home.path().display().to_string()),
            ]),
        };
        let supervisor = TerminalSupervisor::default();
        let session = supervisor
            .spawn_user_shell(&config, &TerminalSize::default(), TerminalConfig::default())
            .expect("spawn zsh");
        session
            .write_input(1, b"print -r -- '__HYPER_'READY'__'\n")
            .unwrap();
        wait_for_tail(&session, b"__HYPER_READY__", Duration::from_secs(5));
        session.write_input(2, b"sleep 5\n").unwrap();
        thread::sleep(Duration::from_millis(150));
        session.write_input(3, b"\x03").unwrap();
        session
            .write_input(4, b"print -r -- '__HYPER_AFTER_'SIGINT'__'; exit\n")
            .unwrap();
        wait_for_tail(&session, b"__HYPER_AFTER_SIGINT__", Duration::from_secs(3));
    }

    #[test]
    fn user_shell_rejects_renderer_ambiguous_paths() {
        let relative = UserShellConfig {
            cwd: Some(PathBuf::from("relative-project")),
            ..UserShellConfig::default()
        };
        assert!(matches!(
            relative.resolved_profile(),
            Err(TerminalError::WorkingDirectoryNotAbsolute(_))
        ));

        let relative_shell = UserShellConfig {
            shell: Some(PathBuf::from("zsh")),
            ..UserShellConfig::default()
        };
        assert!(matches!(
            relative_shell.resolved_profile(),
            Err(TerminalError::ShellPathNotAbsolute(_))
        ));
    }

    #[test]
    fn transcript_tail_trims_overflow_in_bounded_batches() {
        let mut tail = VecDeque::from(b"abcd".to_vec());
        append_transcript_tail(&mut tail, b"ef", 5);
        assert_eq!(tail.iter().copied().collect::<Vec<_>>(), b"bcdef");

        append_transcript_tail(&mut tail, b"0123456789", 5);
        assert_eq!(tail.iter().copied().collect::<Vec<_>>(), b"56789");

        append_transcript_tail(&mut tail, b"XY", 5);
        assert_eq!(tail.iter().copied().collect::<Vec<_>>(), b"789XY");
    }
}
