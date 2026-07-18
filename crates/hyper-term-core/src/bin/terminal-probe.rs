use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use hyper_term_core::{
    TerminalConfig, TerminalEvent, TerminalReplay, TerminalSessionHandle, TerminalSupervisor,
    UserShellConfig,
};
use hyper_term_protocol::{TerminalCommand, TerminalSize};
use serde::Serialize;

const BURST_BYTES: u64 = 8 * 1024 * 1024;
const ECHO_SAMPLES: usize = 64;
const RESIZE_COUNT: u64 = 1_000;

// Initial local regression budgets. They are deliberately emitted with every
// result and must be calibrated on dedicated release hardware before becoming
// a cross-machine product claim.
const STARTUP_BUDGET_MS: f64 = 1_000.0;
const ECHO_P95_BUDGET_MS: f64 = 5.0;
const BURST_BUDGET_MIB_PER_SECOND: f64 = 75.0;
const RESIZE_BUDGET_MS: f64 = 100.0;

#[derive(Serialize)]
struct ProbeReport {
    schema_version: u32,
    build_profile: &'static str,
    operating_system: &'static str,
    architecture: &'static str,
    shell_startup: ShellStartupResult,
    pty_key_to_echo: EchoResult,
    pty_burst: BurstResult,
    resize_storm: ResizeResult,
    all_within_initial_budget: bool,
}

#[derive(Serialize)]
struct ShellStartupResult {
    program: String,
    login: bool,
    startup_to_marker_ms: f64,
    budget_ms: f64,
    within_budget: bool,
}

#[derive(Serialize)]
struct EchoResult {
    samples: usize,
    p50_ms: f64,
    p95_ms: f64,
    maximum_ms: f64,
    p95_budget_ms: f64,
    within_budget: bool,
}

#[derive(Serialize)]
struct BurstResult {
    bytes: u64,
    chunks: u64,
    elapsed_ms: f64,
    mib_per_second: f64,
    minimum_mib_per_second: f64,
    within_budget: bool,
}

#[derive(Serialize)]
struct ResizeResult {
    resize_count: u64,
    elapsed_ms: f64,
    resizes_per_second: f64,
    budget_ms: f64,
    within_budget: bool,
}

fn main() -> anyhow::Result<()> {
    let assert_budget = std::env::args().any(|argument| argument == "--assert-budget");
    let supervisor = TerminalSupervisor::default();
    let shell_startup = probe_shell_startup(&supervisor)?;
    let pty_key_to_echo = probe_key_to_echo(&supervisor)?;
    let pty_burst = probe_burst(&supervisor)?;
    let resize_storm = probe_resize(&supervisor)?;
    let all_within_initial_budget = shell_startup.within_budget
        && pty_key_to_echo.within_budget
        && pty_burst.within_budget
        && resize_storm.within_budget;
    let report = ProbeReport {
        schema_version: 1,
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        shell_startup,
        pty_key_to_echo,
        pty_burst,
        resize_storm,
        all_within_initial_budget,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if assert_budget && !report.all_within_initial_budget {
        bail!("one or more terminal probes exceeded the initial local regression budget");
    }
    Ok(())
}

fn probe_shell_startup(supervisor: &TerminalSupervisor) -> anyhow::Result<ShellStartupResult> {
    let config = UserShellConfig {
        cwd: Some(std::env::current_dir().context("resolve current directory")?),
        ..UserShellConfig::default()
    };
    let profile = config.resolved_profile()?;
    let start = Instant::now();
    let session = supervisor.spawn_user_shell(
        &config,
        &TerminalSize::default(),
        TerminalConfig::default(),
    )?;
    // Split the marker in the source so PTY input echo cannot satisfy the
    // output check before the shell has actually executed the command.
    let marker = b"__HYPER_TERM_READY__";
    session.write_input(1, b"printf '%s\\n' '__HYPER_'TERM_READY'__'; exit\n")?;
    wait_for_tail_marker(&session, marker, Duration::from_secs(15))?;
    let startup_to_marker_ms = elapsed_ms(start.elapsed());
    Ok(ShellStartupResult {
        program: profile.program.display().to_string(),
        login: profile.login,
        startup_to_marker_ms,
        budget_ms: STARTUP_BUDGET_MS,
        within_budget: startup_to_marker_ms <= STARTUP_BUDGET_MS,
    })
}

fn probe_key_to_echo(supervisor: &TerminalSupervisor) -> anyhow::Result<EchoResult> {
    let command = TerminalCommand {
        program: "/bin/cat".into(),
        args: Vec::new(),
        cwd: None,
        env: BTreeMap::from([("TERM".into(), "xterm-256color".into())]),
    };
    let session = supervisor.spawn(
        &command,
        &TerminalSize::default(),
        TerminalConfig::default(),
    )?;
    let subscription = session.subscribe(0);
    let mut output = replay_bytes(subscription.replay);
    let mut samples = Vec::with_capacity(ECHO_SAMPLES);

    for index in 0..ECHO_SAMPLES {
        let marker = format!("__HT_ECHO_{index:04}__");
        let input = format!("{marker}\n");
        let start = Instant::now();
        session.write_input((index + 1) as u64, input.as_bytes())?;
        wait_for_stream_marker(
            &subscription.receiver,
            &mut output,
            marker.as_bytes(),
            Duration::from_secs(2),
        )?;
        samples.push(elapsed_ms(start.elapsed()));
    }
    session.close()?;
    samples.sort_by(f64::total_cmp);
    let p50_ms = percentile(&samples, 50);
    let p95_ms = percentile(&samples, 95);
    let maximum_ms = samples.last().copied().unwrap_or_default();
    Ok(EchoResult {
        samples: samples.len(),
        p50_ms,
        p95_ms,
        maximum_ms,
        p95_budget_ms: ECHO_P95_BUDGET_MS,
        within_budget: p95_ms <= ECHO_P95_BUDGET_MS,
    })
}

fn probe_burst(supervisor: &TerminalSupervisor) -> anyhow::Result<BurstResult> {
    let command = TerminalCommand {
        program: "/usr/bin/head".into(),
        args: vec!["-c".into(), BURST_BYTES.to_string(), "/dev/zero".into()],
        cwd: None,
        env: BTreeMap::new(),
    };
    let start = Instant::now();
    let session = supervisor.spawn(
        &command,
        &TerminalSize::default(),
        TerminalConfig::default(),
    )?;
    let snapshot = wait_for_exit(&session, Duration::from_secs(15))?;
    let elapsed = start.elapsed();
    if snapshot.total_bytes != BURST_BYTES {
        bail!(
            "burst produced {} bytes, expected {BURST_BYTES}",
            snapshot.total_bytes
        );
    }
    let seconds = elapsed.as_secs_f64();
    let mib_per_second = (snapshot.total_bytes as f64 / (1024.0 * 1024.0)) / seconds;
    Ok(BurstResult {
        bytes: snapshot.total_bytes,
        chunks: snapshot.next_sequence.saturating_sub(1),
        elapsed_ms: elapsed_ms(elapsed),
        mib_per_second,
        minimum_mib_per_second: BURST_BUDGET_MIB_PER_SECOND,
        within_budget: mib_per_second >= BURST_BUDGET_MIB_PER_SECOND,
    })
}

fn probe_resize(supervisor: &TerminalSupervisor) -> anyhow::Result<ResizeResult> {
    let command = TerminalCommand {
        program: "/bin/cat".into(),
        args: Vec::new(),
        cwd: None,
        env: BTreeMap::new(),
    };
    let session = supervisor.spawn(
        &command,
        &TerminalSize::default(),
        TerminalConfig::default(),
    )?;
    let start = Instant::now();
    for generation in 1..=RESIZE_COUNT {
        let size = TerminalSize {
            rows: 24 + (generation % 17) as u16,
            cols: 80 + (generation % 41) as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        session.resize(generation, &size)?;
    }
    let elapsed = start.elapsed();
    session.close()?;
    let elapsed_ms = elapsed_ms(elapsed);
    Ok(ResizeResult {
        resize_count: RESIZE_COUNT,
        elapsed_ms,
        resizes_per_second: RESIZE_COUNT as f64 / elapsed.as_secs_f64(),
        budget_ms: RESIZE_BUDGET_MS,
        within_budget: elapsed_ms <= RESIZE_BUDGET_MS,
    })
}

fn replay_bytes(replay: TerminalReplay) -> Vec<u8> {
    match replay {
        TerminalReplay::Chunks(chunks) => chunks
            .into_iter()
            .flat_map(|chunk| chunk.bytes.iter().copied().collect::<Vec<_>>())
            .collect(),
        TerminalReplay::SnapshotRequired(snapshot) => snapshot.tail,
    }
}

fn wait_for_stream_marker(
    receiver: &crossbeam_channel::Receiver<TerminalEvent>,
    output: &mut Vec<u8>,
    marker: &[u8],
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if contains(output, marker) {
            return Ok(());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow!("PTY echo marker timed out"))?;
        match receiver.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(TerminalEvent::Output(chunk)) => output.extend_from_slice(&chunk.bytes),
            Ok(TerminalEvent::Fault(message)) => bail!("PTY fault: {message}"),
            Ok(TerminalEvent::Exited(exit)) => bail!("PTY exited during echo probe: {exit:?}"),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn wait_for_tail_marker(
    session: &TerminalSessionHandle,
    marker: &[u8],
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = session.snapshot();
        if contains(&snapshot.tail, marker) {
            return Ok(());
        }
        if snapshot.exit.is_some() {
            bail!("shell exited before emitting startup marker");
        }
        if Instant::now() >= deadline {
            bail!("shell startup marker timed out");
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for_exit(
    session: &TerminalSessionHandle,
    timeout: Duration,
) -> anyhow::Result<hyper_term_core::TerminalSnapshot> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = session.snapshot();
        if snapshot.exit.is_some() {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            bail!("terminal exit timed out");
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn percentile(sorted: &[f64], percentile: usize) -> f64 {
    let index = sorted.len().saturating_sub(1).saturating_mul(percentile) / 100;
    sorted.get(index).copied().unwrap_or_default()
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
