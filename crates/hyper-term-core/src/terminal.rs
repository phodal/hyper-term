use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use hyper_term_protocol::{TerminalCommand, TerminalId, TerminalSize};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use thiserror::Error;

const READER_CHUNK_BYTES: usize = 16 * 1024;
const SUBSCRIBER_QUEUE_CHUNKS: usize = 256;

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

        let pair = native_pty_system().openpty(to_pty_size(size))?;
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        if let Some(cwd) = &command.cwd {
            builder.cwd(cwd);
        }
        for (key, value) in &command.env {
            builder.env(key, value);
        }

        let mut child = pair.slave.spawn_command(builder)?;
        let process_id = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
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
        thread::Builder::new()
            .name(format!("terminal-reader-{terminal_id}"))
            .spawn(move || {
                let mut buffer = vec![0_u8; READER_CHUNK_BYTES];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(length) => reader_replay.publish_output(&buffer[..length]),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            reader_replay.publish_fault(format!("PTY read failed: {error}"));
                            break;
                        }
                    }
                }
            })?;

        let waiter_replay = Arc::clone(&replay);
        thread::Builder::new()
            .name(format!("terminal-wait-{terminal_id}"))
            .spawn(move || match child.wait() {
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
        state.transcript_tail.extend(bytes);
        while state.transcript_tail.len() > self.transcript_tail_bytes {
            state.transcript_tail.pop_front();
        }
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

    fn shell(script: &str) -> TerminalCommand {
        TerminalCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: BTreeMap::new(),
        }
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
}
