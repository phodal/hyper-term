//! Rust-owned desktop supervisor for the packaged Native SDK renderer.
//!
//! The supervisor is the `.app` entry point. It owns daemon lifetime, the
//! authenticated loopback gateway, state paths, and the native renderer child.
//! The renderer still never spawns shells or receives a privileged bridge.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
use hyper_term_daemon::{
    AcpAgentProviderConfig, AgentGatewayConfig, AgentGenUiRuntimeConfig, DaemonState,
    TerminalGatewayConfig, spawn_agent_gateway, spawn_terminal_gateway, spawn_unix_server,
};
use uuid::Uuid;

use hyper_term_drivers::sha256_file;
use serde::{Deserialize, Serialize};

const DESKTOP_TERMINAL_ADDRESS: &str = "127.0.0.1:47437";
const TERMINAL_URL_ENV: &str = "HYPER_TERM_TERMINAL_URL";
const AGENT_URL_ENV: &str = "HYPER_TERM_AGENT_URL";
const AGENT_PROVIDERS_ENV: &str = "HYPER_TERM_AGENT_PROVIDERS";
const AGENT_PROVIDER_STATUS_ENV: &str = "HYPER_TERM_AGENT_PROVIDER_STATUS";
const PROVIDER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROVIDER_PROBE_MAX_STDOUT_BYTES: usize = 4 * 1024;
const ACP_PACKAGE_MANIFEST_MAX_BYTES: u64 = 64 * 1024;
const ACP_RUNTIME_MANIFEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
const ACP_RUNTIME_MAX_FILES: usize = 8 * 1024;
const ACP_RUNTIME_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const DESKTOP_HELP: &str = "Hyper Term desktop host\n\nUsage: hyper-term-desktop [OPTIONS]\n\n\
Options:\n  --ui PATH                 Native renderer executable\n  \
--terminal-assets PATH    Built terminal renderer directory\n  \
--workbench-assets PATH   Built trusted artifact Workbench directory\n  \
--state-dir PATH          Durable Hyper Term state\n  \
--shell-cwd PATH          Initial directory for new shells\n  \
--codex PATH              Codex executable for Agent sessions\n  \
--codex-auth PATH         Private Codex auth.json for isolated Agent sessions\n  \
--codex-acp PATH          Codex ACP adapter executable\n  \
--claude-agent-acp PATH   Claude Agent ACP adapter executable\n  \
--claude PATH             Claude Code executable used by Claude ACP\n  \
--copilot PATH            GitHub Copilot CLI used through ACP\n  \
--deno-runtime PATH       Pinned Deno executable for brokered Agent tools\n  \
--genui-script PATH       Bundled GenUI compiler service\n  \
--genui-wasm PATH         Pinned esbuild-wasm compiler binary\n  \
--genui-preview PATH      Bundled isolated GenUI preview capsule\n  \
-h, --help                Show this help";

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("hyper-term-desktop: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(unix)]
fn run() -> Result<i32, String> {
    let options = Options::parse(std::env::args_os().skip(1))?;
    if options.help {
        println!("{DESKTOP_HELP}");
        return Ok(0);
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot resolve desktop executable: {error}"))?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not available for desktop state and shell cwd".to_owned())?;
    let resolved = options.resolve(&executable, &home)?;
    resolved.validate()?;
    std::fs::create_dir_all(&resolved.state_directory)
        .map_err(|error| format!("cannot create desktop state directory: {error}"))?;

    let daemon = DaemonState::open(&resolved.state_directory).map_err(|error| error.to_string())?;
    let control_socket = resolved.state_directory.join("hyperd.sock");
    let control_server = spawn_unix_server(control_socket.clone(), daemon.clone())
        .map_err(|error| error.to_string())?;
    let terminal_token = desktop_token();
    let agent_token = desktop_token();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("hyper-term-desktop")
        .build()
        .map_err(|error| error.to_string())?;

    let result = runtime.block_on(async {
        let gateway = spawn_terminal_gateway(
            TerminalGatewayConfig {
                bind: DESKTOP_TERMINAL_ADDRESS
                    .parse()
                    .expect("desktop terminal address is valid"),
                assets: resolved.terminal_assets.clone(),
                token: terminal_token.clone(),
                default_cwd: Some(resolved.shell_cwd.clone()),
            },
            daemon.clone(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let provider_inventory = resolved.agent_provider_inventory(&home)?;
        let agent_provider_ids = provider_inventory
            .iter()
            .filter(|provider| provider.usable())
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let provider_status = serde_json::to_string(&provider_inventory)
            .map_err(|error| format!("cannot serialize Agent provider status: {error}"))?;
        let codex_ready = provider_inventory.iter().any(|provider| {
            provider.id == "codex" && provider.readiness == AgentProviderReadiness::Authenticated
        });
        let ready_acp_providers = provider_inventory
            .iter()
            .filter(|provider| provider.usable())
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>();
        let acp_providers = resolved
            .acp_providers(&home)?
            .into_iter()
            .filter(|provider| ready_acp_providers.contains(&provider.provider_id.as_str()))
            .collect();
        let agent_gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("agent loopback bind is valid"),
            token: agent_token.clone(),
            workspace: resolved.shell_cwd.clone(),
            state_directory: resolved.state_directory.join("agent-runtime"),
            daemon: daemon.clone(),
            codex_executable: codex_ready.then(|| resolved.codex.clone()).flatten(),
            codex_auth_file: codex_ready.then(|| resolved.codex_auth.clone()).flatten(),
            acp_providers,
            mcp_executable: resolved.mcp.clone(),
            genui_runtime: resolved
                .genui_runtime
                .as_ref()
                .map(|runtime| AgentGenUiRuntimeConfig {
                    deno_executable: runtime.deno_executable.clone(),
                    runtime_version: "2.9.3".into(),
                    compiler_script: runtime.compiler_script.clone(),
                    compiler_wasm: runtime.compiler_wasm.clone(),
                    preview_shell: runtime.preview_shell.clone(),
                    compiler_version: "0.28.1".into(),
                }),
            workbench_assets: resolved.workbench_assets.clone(),
            control_socket,
        })
        .await
        .map_err(|error| error.to_string())?;
        let terminal_url = format!("http://{}/?token={terminal_token}", gateway.address());
        let agent_url = format!("http://{}/?token={agent_token}", agent_gateway.address());
        let mut renderer = Command::new(&resolved.ui)
            .env(TERMINAL_URL_ENV, terminal_url)
            .env(AGENT_URL_ENV, agent_url)
            .env(AGENT_PROVIDERS_ENV, agent_provider_ids)
            .env(AGENT_PROVIDER_STATUS_ENV, provider_status)
            .spawn()
            .map_err(|error| format!("cannot start native renderer: {error}"))?;
        let status = wait_for_renderer(&mut renderer).await?;
        agent_gateway
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        gateway
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok::<ExitStatus, String>(status)
    });
    drop(control_server);
    let status = result?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(not(unix))]
fn run() -> Result<i32, String> {
    Err("the desktop host currently requires a Unix PTY platform".into())
}

#[cfg(unix)]
async fn wait_for_renderer(renderer: &mut std::process::Child) -> Result<ExitStatus, String> {
    wait_for_renderer_until(renderer, desktop_shutdown_signal()).await
}

#[cfg(unix)]
async fn desktop_shutdown_signal() -> Result<(), String> {
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|error| format!("cannot listen for desktop termination: {error}"))?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.map_err(|error| format!("cannot wait for desktop interrupt: {error}"))
        }
        result = terminate.recv() => {
            result.ok_or_else(|| "desktop termination signal stream closed".to_owned())
        }
    }
}

#[cfg(unix)]
async fn wait_for_renderer_until(
    renderer: &mut std::process::Child,
    shutdown: impl Future<Output = Result<(), String>>,
) -> Result<ExitStatus, String> {
    tokio::pin!(shutdown);
    loop {
        if let Some(status) = renderer
            .try_wait()
            .map_err(|error| format!("cannot inspect native renderer: {error}"))?
        {
            return Ok(status);
        }
        tokio::select! {
            result = &mut shutdown => {
                result?;
                let _ = renderer.kill();
                return renderer
                    .wait()
                    .map_err(|error| format!("cannot stop native renderer: {error}"));
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct Options {
    ui: Option<PathBuf>,
    terminal_assets: Option<PathBuf>,
    workbench_assets: Option<PathBuf>,
    state_directory: Option<PathBuf>,
    shell_cwd: Option<PathBuf>,
    codex: Option<PathBuf>,
    codex_auth: Option<PathBuf>,
    codex_acp: Option<PathBuf>,
    claude_agent_acp: Option<PathBuf>,
    claude: Option<PathBuf>,
    copilot: Option<PathBuf>,
    deno_runtime: Option<PathBuf>,
    genui_script: Option<PathBuf>,
    genui_wasm: Option<PathBuf>,
    genui_preview: Option<PathBuf>,
    help: bool,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--ui") => options.ui = Some(required_path(&mut arguments, "--ui")?),
                Some("--terminal-assets") => {
                    options.terminal_assets =
                        Some(required_path(&mut arguments, "--terminal-assets")?);
                }
                Some("--workbench-assets") => {
                    options.workbench_assets =
                        Some(required_path(&mut arguments, "--workbench-assets")?);
                }
                Some("--state-dir") => {
                    options.state_directory = Some(required_path(&mut arguments, "--state-dir")?);
                }
                Some("--shell-cwd") => {
                    options.shell_cwd = Some(required_path(&mut arguments, "--shell-cwd")?);
                }
                Some("--codex") => options.codex = Some(required_path(&mut arguments, "--codex")?),
                Some("--codex-auth") => {
                    options.codex_auth = Some(required_path(&mut arguments, "--codex-auth")?);
                }
                Some("--codex-acp") => {
                    options.codex_acp = Some(required_path(&mut arguments, "--codex-acp")?);
                }
                Some("--claude-agent-acp") => {
                    options.claude_agent_acp =
                        Some(required_path(&mut arguments, "--claude-agent-acp")?);
                }
                Some("--claude") => {
                    options.claude = Some(required_path(&mut arguments, "--claude")?);
                }
                Some("--copilot") => {
                    options.copilot = Some(required_path(&mut arguments, "--copilot")?);
                }
                Some("--deno-runtime") => {
                    options.deno_runtime = Some(required_path(&mut arguments, "--deno-runtime")?);
                }
                Some("--genui-script") => {
                    options.genui_script = Some(required_path(&mut arguments, "--genui-script")?);
                }
                Some("--genui-wasm") => {
                    options.genui_wasm = Some(required_path(&mut arguments, "--genui-wasm")?);
                }
                Some("--genui-preview") => {
                    options.genui_preview = Some(required_path(&mut arguments, "--genui-preview")?);
                }
                Some("-h" | "--help") => options.help = true,
                Some(other) => return Err(format!("unknown argument: {other}")),
                None => return Err("desktop arguments must be valid UTF-8 option names".into()),
            }
        }
        Ok(options)
    }

    fn resolve(self, executable: &Path, home: &Path) -> Result<ResolvedOptions, String> {
        let contents = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "desktop executable is not inside a macOS bundle layout".to_owned())?;
        let runtime_resources = contents.join("Resources/runtime");
        let explicit_workbench = self.workbench_assets.is_some();
        let workbench_candidate = self
            .workbench_assets
            .unwrap_or_else(|| contents.join("Resources/workbench"));
        let workbench_assets = (explicit_workbench
            || workbench_candidate.join("index.html").is_file())
        .then_some(workbench_candidate);
        let explicit_runtime = self.deno_runtime.is_some()
            || self.genui_script.is_some()
            || self.genui_wasm.is_some()
            || self.genui_preview.is_some();
        let deno_executable = self
            .deno_runtime
            .unwrap_or_else(|| runtime_resources.join("deno"));
        let compiler_script = self
            .genui_script
            .unwrap_or_else(|| runtime_resources.join("genui-compiler.js"));
        let compiler_wasm = self
            .genui_wasm
            .unwrap_or_else(|| runtime_resources.join("esbuild.wasm"));
        let preview_shell = self
            .genui_preview
            .unwrap_or_else(|| runtime_resources.join("genui/preview.html"));
        let complete_runtime = deno_executable.is_file()
            && compiler_script.is_file()
            && compiler_wasm.is_file()
            && preview_shell.is_file();
        let genui_runtime =
            (explicit_runtime || complete_runtime).then_some(ResolvedGenUiRuntime {
                deno_executable: deno_executable.clone(),
                compiler_script,
                compiler_wasm,
                preview_shell,
            });
        let codex = self
            .codex
            .or_else(|| std::env::var_os("HYPER_TERM_CODEX_PATH").map(PathBuf::from))
            .or_else(|| find_executable("codex", home));
        let claude = self
            .claude
            .or_else(|| std::env::var_os("HYPER_TERM_CLAUDE_PATH").map(PathBuf::from))
            .or_else(|| find_executable("claude", home));
        let copilot = self
            .copilot
            .or_else(|| std::env::var_os("HYPER_TERM_COPILOT_PATH").map(PathBuf::from))
            .or_else(|| find_executable("copilot", home));
        let bundled_acp = load_bundled_acp_runtime(&runtime_resources, &deno_executable)?;
        let codex_acp = self
            .codex_acp
            .or_else(|| std::env::var_os("HYPER_TERM_CODEX_ACP_PATH").map(PathBuf::from))
            .map(ResolvedAcpAdapter::installed)
            .or_else(|| discover_known_acp_adapter("codex-acp", home))
            .or_else(|| {
                codex
                    .is_some()
                    .then(|| bundled_acp.get("codex-acp").cloned())
                    .flatten()
            });
        let claude_agent_acp = self
            .claude_agent_acp
            .or_else(|| std::env::var_os("HYPER_TERM_CLAUDE_AGENT_ACP_PATH").map(PathBuf::from))
            .map(ResolvedAcpAdapter::installed)
            .or_else(|| discover_known_acp_adapter("claude-agent-acp", home))
            .or_else(|| {
                claude
                    .is_some()
                    .then(|| bundled_acp.get("claude-acp").cloned())
                    .flatten()
            });
        Ok(ResolvedOptions {
            ui: self
                .ui
                .unwrap_or_else(|| executable.with_file_name("hyper-term-ui")),
            terminal_assets: self
                .terminal_assets
                .unwrap_or_else(|| contents.join("Resources/terminal")),
            workbench_assets,
            state_directory: self
                .state_directory
                .unwrap_or_else(|| default_state_directory(home)),
            shell_cwd: self.shell_cwd.unwrap_or_else(|| home.to_owned()),
            codex,
            codex_auth: self.codex_auth.or_else(|| {
                let candidate = home.join(".codex/auth.json");
                candidate.is_file().then_some(candidate)
            }),
            codex_acp,
            claude_agent_acp,
            claude,
            copilot,
            mcp: executable
                .with_file_name("hyper-term-mcp")
                .is_file()
                .then(|| executable.with_file_name("hyper-term-mcp")),
            genui_runtime,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ResolvedOptions {
    ui: PathBuf,
    terminal_assets: PathBuf,
    workbench_assets: Option<PathBuf>,
    state_directory: PathBuf,
    shell_cwd: PathBuf,
    codex: Option<PathBuf>,
    codex_auth: Option<PathBuf>,
    codex_acp: Option<ResolvedAcpAdapter>,
    claude_agent_acp: Option<ResolvedAcpAdapter>,
    claude: Option<PathBuf>,
    copilot: Option<PathBuf>,
    mcp: Option<PathBuf>,
    genui_runtime: Option<ResolvedGenUiRuntime>,
}

#[derive(Debug, Eq, PartialEq)]
struct ResolvedGenUiRuntime {
    deno_executable: PathBuf,
    compiler_script: PathBuf,
    compiler_wasm: PathBuf,
    preview_shell: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedAcpAdapter {
    executable: PathBuf,
    arguments: Vec<OsString>,
    implementation_version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentProviderReadiness {
    Authenticated,
    Available,
    LoginRequired,
    ProviderMissing,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentProviderContainment {
    ExternalEnforcementPending,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DesktopAgentProviderStatus {
    id: String,
    protocol: String,
    readiness: AgentProviderReadiness,
    containment: AgentProviderContainment,
}

impl DesktopAgentProviderStatus {
    fn new(id: &str, protocol: &str, readiness: AgentProviderReadiness) -> Self {
        Self {
            id: id.into(),
            protocol: protocol.into(),
            readiness,
            containment: AgentProviderContainment::ExternalEnforcementPending,
        }
    }

    fn usable(&self) -> bool {
        matches!(
            self.readiness,
            AgentProviderReadiness::Authenticated | AgentProviderReadiness::Available
        )
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ProbeOutcome {
    Exited { success: bool, stdout: Vec<u8> },
    TimedOut,
}

impl ResolvedAcpAdapter {
    fn installed(executable: PathBuf) -> Self {
        Self::installed_version(executable, "installed")
    }

    fn installed_version(executable: PathBuf, implementation_version: impl Into<String>) -> Self {
        Self {
            executable,
            arguments: Vec::new(),
            implementation_version: implementation_version.into(),
        }
    }
}

impl ResolvedOptions {
    fn validate(&self) -> Result<(), String> {
        validate_executable(&self.ui)?;
        if !self.terminal_assets.join("index.html").is_file() {
            return Err(format!(
                "terminal assets are missing index.html: {}",
                self.terminal_assets.display()
            ));
        }
        if let Some(workbench) = &self.workbench_assets
            && (!workbench.is_absolute() || !workbench.join("index.html").is_file())
        {
            return Err(format!(
                "Workbench assets are missing index.html: {}",
                workbench.display()
            ));
        }
        if !self.shell_cwd.is_absolute() || !self.shell_cwd.is_dir() {
            return Err(format!(
                "initial shell directory is not an absolute directory: {}",
                self.shell_cwd.display()
            ));
        }
        if let Some(codex) = &self.codex {
            validate_executable(codex)?;
        }
        if let Some(codex_auth) = &self.codex_auth
            && (!codex_auth.is_absolute() || !codex_auth.is_file())
        {
            return Err(format!(
                "Codex auth file is unavailable: {}",
                codex_auth.display()
            ));
        }
        if let Some(codex_acp) = &self.codex_acp {
            validate_executable(&codex_acp.executable)?;
        }
        if let Some(claude_agent_acp) = &self.claude_agent_acp {
            validate_executable(&claude_agent_acp.executable)?;
        }
        if let Some(claude) = &self.claude {
            validate_executable(claude)?;
        }
        if let Some(copilot) = &self.copilot {
            validate_executable(copilot)?;
        }
        if let Some(mcp) = &self.mcp {
            validate_executable(mcp)?;
        }
        if let Some(runtime) = &self.genui_runtime {
            validate_executable(&runtime.deno_executable)?;
            for asset in [
                &runtime.compiler_script,
                &runtime.compiler_wasm,
                &runtime.preview_shell,
            ] {
                if !asset.is_absolute() || !asset.is_file() {
                    return Err(format!(
                        "GenUI runtime asset is unavailable: {}",
                        asset.display()
                    ));
                }
            }
        }
        Ok(())
    }

    fn agent_provider_inventory(
        &self,
        home: &Path,
    ) -> Result<Vec<DesktopAgentProviderStatus>, String> {
        let codex_readiness = self
            .codex
            .as_deref()
            .map(|executable| {
                probe_agent_authentication(
                    executable,
                    &["login", "status"],
                    home,
                    self.codex_auth.as_deref().and_then(Path::parent),
                )
            })
            .transpose()?
            .unwrap_or(AgentProviderReadiness::ProviderMissing);
        let claude_readiness = self
            .claude
            .as_deref()
            .map(|executable| {
                probe_agent_authentication(executable, &["auth", "status"], home, None)
            })
            .transpose()?
            .unwrap_or(AgentProviderReadiness::ProviderMissing);
        let copilot_readiness = self
            .copilot
            .as_deref()
            .map(probe_copilot_availability)
            .transpose()?
            .unwrap_or(AgentProviderReadiness::ProviderMissing);

        let mut providers = Vec::with_capacity(4);
        if self.codex.is_some() {
            providers.push(DesktopAgentProviderStatus::new(
                "codex",
                "codex-app-server-v2",
                codex_readiness,
            ));
        }
        if self.codex_acp.is_some() {
            providers.push(DesktopAgentProviderStatus::new(
                "codex-acp",
                "acp-v1",
                codex_readiness,
            ));
        }
        if self.claude_agent_acp.is_some() {
            providers.push(DesktopAgentProviderStatus::new(
                "claude-acp",
                "acp-v1",
                claude_readiness,
            ));
        }
        if self.copilot.is_some() {
            providers.push(DesktopAgentProviderStatus::new(
                "copilot-acp",
                "acp-v1",
                copilot_readiness,
            ));
        }
        Ok(providers)
    }

    fn acp_providers(&self, home: &Path) -> Result<Vec<AcpAgentProviderConfig>, String> {
        let mut providers = Vec::with_capacity(3);
        if let Some(adapter) = &self.codex_acp {
            let mut environment = acp_environment(home, &adapter.executable)?;
            environment.insert("NO_BROWSER".into(), "1".into());
            environment.insert("DENO_NO_UPDATE_CHECK".into(), "1".into());
            environment.insert("DENO_NO_PROMPT".into(), "1".into());
            if let Some(codex) = &self.codex {
                environment.insert("CODEX_PATH".into(), codex.as_os_str().to_owned());
            }
            providers.push(AcpAgentProviderConfig {
                provider_id: "codex-acp".into(),
                executable: adapter.executable.clone(),
                arguments: adapter.arguments.clone(),
                environment,
                implementation_version: adapter.implementation_version.clone(),
            });
        }
        if let Some(adapter) = &self.claude_agent_acp {
            let mut environment = acp_environment(home, &adapter.executable)?;
            environment.insert("DENO_NO_UPDATE_CHECK".into(), "1".into());
            environment.insert("DENO_NO_PROMPT".into(), "1".into());
            if let Some(claude) = &self.claude {
                environment.insert(
                    "CLAUDE_CODE_EXECUTABLE".into(),
                    claude.as_os_str().to_owned(),
                );
            }
            providers.push(AcpAgentProviderConfig {
                provider_id: "claude-acp".into(),
                executable: adapter.executable.clone(),
                arguments: adapter.arguments.clone(),
                environment,
                implementation_version: adapter.implementation_version.clone(),
            });
        }
        if let Some(copilot) = &self.copilot {
            let environment = acp_environment(home, copilot)?;
            providers.push(AcpAgentProviderConfig {
                provider_id: "copilot-acp".into(),
                executable: copilot.clone(),
                arguments: [
                    "--acp",
                    "--stdio",
                    "--no-auto-update",
                    "--no-remote",
                    "--no-remote-export",
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
                environment,
                implementation_version: "installed".into(),
            });
        }
        Ok(providers)
    }
}

#[derive(Deserialize)]
struct BundledAcpRuntimeManifest {
    schema_version: u32,
    runtime: BundledAcpRuntimeIdentity,
    adapters: Vec<BundledAcpAdapterManifest>,
    files: Vec<BundledAcpFileManifest>,
}

#[derive(Deserialize)]
struct BundledAcpRuntimeIdentity {
    name: String,
    version: String,
}

#[derive(Deserialize)]
struct BundledAcpAdapterManifest {
    provider_id: String,
    package: String,
    version: String,
    entrypoint: String,
    required_agent: String,
    entrypoint_sha256: String,
}

#[derive(Deserialize)]
struct BundledAcpFileManifest {
    path: String,
    bytes: u64,
    sha256: String,
}

fn load_bundled_acp_runtime(
    runtime_resources: &Path,
    deno_executable: &Path,
) -> Result<BTreeMap<String, ResolvedAcpAdapter>, String> {
    let root = runtime_resources.join("acp");
    let manifest_path = root.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(BTreeMap::new());
    }
    validate_executable(deno_executable)?;
    let manifest_metadata = std::fs::metadata(&manifest_path)
        .map_err(|error| format!("cannot inspect bundled ACP manifest: {error}"))?;
    if manifest_metadata.len() == 0 || manifest_metadata.len() > ACP_RUNTIME_MANIFEST_MAX_BYTES {
        return Err("bundled ACP manifest size is invalid".into());
    }
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("cannot read bundled ACP manifest: {error}"))?;
    let manifest: BundledAcpRuntimeManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("cannot parse bundled ACP manifest: {error}"))?;
    if manifest.schema_version != 1
        || manifest.runtime.name != "deno"
        || manifest.runtime.version != "2.9.3"
    {
        return Err("bundled ACP runtime identity is unsupported".into());
    }
    if manifest.files.is_empty() || manifest.files.len() > ACP_RUNTIME_MAX_FILES {
        return Err("bundled ACP file inventory is invalid".into());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve bundled ACP runtime: {error}"))?;
    let mut file_digests = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for file in manifest.files {
        let relative_path = validate_bundled_relative_path(&file.path)?;
        if !is_sha256(&file.sha256) {
            return Err(format!(
                "bundled ACP file metadata is invalid: {}",
                file.path
            ));
        }
        total_bytes = total_bytes
            .checked_add(file.bytes)
            .ok_or_else(|| "bundled ACP runtime byte count overflowed".to_owned())?;
        if total_bytes > ACP_RUNTIME_MAX_TOTAL_BYTES {
            return Err("bundled ACP runtime exceeds its byte budget".into());
        }
        let path = canonical_root.join(&relative_path);
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("bundled ACP file is unavailable: {}: {error}", file.path))?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(format!(
                "bundled ACP file escapes its runtime: {}",
                file.path
            ));
        }
        let metadata = std::fs::metadata(&canonical_path)
            .map_err(|error| format!("cannot inspect bundled ACP file {}: {error}", file.path))?;
        if !metadata.is_file() || metadata.len() != file.bytes {
            return Err(format!("bundled ACP file size changed: {}", file.path));
        }
        let actual = sha256_file(&canonical_path)
            .map_err(|error| format!("cannot hash bundled ACP file {}: {error}", file.path))?;
        if actual != file.sha256 {
            return Err(format!("bundled ACP file digest changed: {}", file.path));
        }
        if file_digests
            .insert(file.path.clone(), file.sha256)
            .is_some()
        {
            return Err(format!("bundled ACP file is duplicated: {}", file.path));
        }
    }

    if manifest.adapters.len() != 2 {
        return Err("bundled ACP adapter inventory is incomplete".into());
    }
    let mut adapters = BTreeMap::new();
    for adapter in manifest.adapters {
        let expected_package = match adapter.provider_id.as_str() {
            "codex-acp" if adapter.required_agent == "codex" => "@agentclientprotocol/codex-acp",
            "claude-acp" if adapter.required_agent == "claude" => {
                "@agentclientprotocol/claude-agent-acp"
            }
            _ => return Err("bundled ACP adapter identity is unsupported".into()),
        };
        if adapter.package != expected_package || adapter.version.is_empty() {
            return Err(format!(
                "bundled ACP package identity changed: {}",
                adapter.provider_id
            ));
        }
        let entrypoint = validate_bundled_relative_path(&adapter.entrypoint)?;
        let Some(file_digest) = file_digests.get(&adapter.entrypoint) else {
            return Err(format!(
                "bundled ACP entrypoint is not inventoried: {}",
                adapter.provider_id
            ));
        };
        if file_digest != &adapter.entrypoint_sha256 || !is_sha256(file_digest) {
            return Err(format!(
                "bundled ACP entrypoint digest changed: {}",
                adapter.provider_id
            ));
        }
        let entrypoint = canonical_root.join(entrypoint);
        let resolved = ResolvedAcpAdapter {
            executable: deno_executable.to_owned(),
            arguments: [
                "run",
                "--cached-only",
                "--no-config",
                "--node-modules-dir=manual",
                "-A",
            ]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::once(entrypoint.into_os_string()))
            .collect(),
            implementation_version: adapter.version,
        };
        if adapters
            .insert(adapter.provider_id.clone(), resolved)
            .is_some()
        {
            return Err(format!(
                "bundled ACP adapter is duplicated: {}",
                adapter.provider_id
            ));
        }
    }
    Ok(adapters)
}

fn validate_bundled_relative_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.len() > 4096 {
        return Err("bundled ACP path length is invalid".into());
    }
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "bundled ACP path must be normalized and relative: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn acp_environment(home: &Path, executable: &Path) -> Result<BTreeMap<String, OsString>, String> {
    let mut path_entries = Vec::with_capacity(5);
    if let Some(parent) = executable.parent() {
        path_entries.push(parent.to_owned());
    }
    for path in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        let path = PathBuf::from(path);
        if !path_entries.contains(&path) {
            path_entries.push(path);
        }
    }
    let path = std::env::join_paths(path_entries)
        .map_err(|error| format!("cannot construct ACP runtime PATH: {error}"))?;
    Ok(BTreeMap::from([
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), path),
        ("TERM".into(), "dumb".into()),
        ("USER".into(), desktop_user_name(home)),
        ("LOGNAME".into(), desktop_user_name(home)),
    ]))
}

fn probe_agent_authentication(
    executable: &Path,
    arguments: &[&str],
    home: &Path,
    tool_home: Option<&Path>,
) -> Result<AgentProviderReadiness, String> {
    let mut environment = acp_environment(home, executable)?;
    if let Some(tool_home) = tool_home {
        environment.insert("CODEX_HOME".into(), tool_home.as_os_str().to_owned());
    }
    match run_bounded_probe(
        executable,
        arguments,
        Some(&environment),
        PROVIDER_PROBE_TIMEOUT,
    )? {
        ProbeOutcome::Exited { success: true, .. } => Ok(AgentProviderReadiness::Authenticated),
        ProbeOutcome::Exited { success: false, .. } => Ok(AgentProviderReadiness::LoginRequired),
        ProbeOutcome::TimedOut => Ok(AgentProviderReadiness::ProbeFailed),
    }
}

fn probe_copilot_availability(executable: &Path) -> Result<AgentProviderReadiness, String> {
    match run_bounded_probe(executable, &["--version"], None, PROVIDER_PROBE_TIMEOUT)? {
        ProbeOutcome::Exited {
            success: true,
            stdout,
        } if String::from_utf8_lossy(&stdout).contains("GitHub Copilot CLI") => {
            // Copilot has no read-only login-status command. Its ACP server
            // advertises and performs authentication as part of the session.
            Ok(AgentProviderReadiness::Available)
        }
        ProbeOutcome::Exited { .. } | ProbeOutcome::TimedOut => {
            Ok(AgentProviderReadiness::ProbeFailed)
        }
    }
}

fn run_bounded_probe(
    executable: &Path,
    arguments: &[&str],
    environment: Option<&BTreeMap<String, OsString>>,
    timeout: Duration,
) -> Result<ProbeOutcome, String> {
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
                break ProbeOutcome::Exited {
                    success: status.success(),
                    stdout: Vec::new(),
                };
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                terminate_probe(&mut child);
                break ProbeOutcome::TimedOut;
            }
            Err(error) => {
                terminate_probe(&mut child);
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
        ProbeOutcome::Exited { success, .. } => ProbeOutcome::Exited {
            success,
            stdout: retained,
        },
        ProbeOutcome::TimedOut => ProbeOutcome::TimedOut,
    })
}

fn terminate_probe(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The probe starts in its own process group, so a stuck adapter cannot
        // keep inherited stdout open through an unobserved descendant.
        let process_group = -(child.id() as i32);
        // SAFETY: `process_group` names only the child group created above.
        unsafe {
            libc::kill(process_group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn desktop_user_name(home: &Path) -> OsString {
    std::env::var_os("USER")
        .or_else(|| std::env::var_os("LOGNAME"))
        .or_else(|| home.file_name().map(OsStr::to_owned))
        .unwrap_or_else(|| "hyper-term".into())
}

fn required_path(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{option} requires a path"))
}

fn default_state_directory(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Hyper Term")
    } else {
        home.join(".local/state/hyper-term")
    }
}

fn desktop_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[cfg(unix)]
fn find_executable(name: &str, home: &Path) -> Option<PathBuf> {
    find_executable_in(name, home, std::env::var_os("PATH").as_deref())
}

#[cfg(unix)]
fn discover_known_acp_adapter(name: &str, home: &Path) -> Option<ResolvedAcpAdapter> {
    let executable = find_executable(name, home)?;
    resolve_known_acp_adapter(executable, name)
}

#[cfg(unix)]
fn resolve_known_acp_adapter(executable: PathBuf, name: &str) -> Option<ResolvedAcpAdapter> {
    let version = match name {
        "codex-acp" => known_npm_acp_package_version(
            &executable,
            "codex-acp",
            &[
                "@zed-industries/codex-acp",
                "@agentclientprotocol/codex-acp",
            ],
        ),
        "claude-agent-acp" => known_npm_acp_package_version(
            &executable,
            "claude-agent-acp",
            &["@agentclientprotocol/claude-agent-acp"],
        ),
        _ => None,
    }?;
    Some(ResolvedAcpAdapter::installed_version(executable, version))
}

#[derive(Deserialize)]
struct NpmAcpPackageManifest {
    name: String,
    version: String,
    bin: BTreeMap<String, String>,
}

#[cfg(unix)]
fn known_npm_acp_package_version(
    executable: &Path,
    bin_name: &str,
    known_packages: &[&str],
) -> Option<String> {
    let executable = executable.canonicalize().ok()?;
    let executable_parent = executable.parent()?;
    let package_root = [executable_parent, executable_parent.parent()?]
        .into_iter()
        .find(|directory| directory.join("package.json").exists())?;
    let manifest_path = package_root.join("package.json");
    let manifest_metadata = std::fs::symlink_metadata(&manifest_path).ok()?;
    if manifest_metadata.file_type().is_symlink()
        || !manifest_metadata.is_file()
        || manifest_metadata.len() == 0
        || manifest_metadata.len() > ACP_PACKAGE_MANIFEST_MAX_BYTES
    {
        return None;
    }
    let manifest_bytes = std::fs::read(&manifest_path).ok()?;
    let manifest: NpmAcpPackageManifest = serde_json::from_slice(&manifest_bytes).ok()?;
    if !known_packages.contains(&manifest.name.as_str())
        || !npm_package_root_matches(package_root, &manifest.name)
        || !valid_package_version(&manifest.version)
    {
        return None;
    }
    let declared_bin = validate_bundled_relative_path(manifest.bin.get(bin_name)?).ok()?;
    let declared_executable = package_root.join(declared_bin).canonicalize().ok()?;
    if declared_executable != executable {
        return None;
    }
    Some(format!("{}@{}", manifest.name, manifest.version))
}

fn npm_package_root_matches(package_root: &Path, package_name: &str) -> bool {
    let mut package_segments = package_name.split('/');
    let Some(scope) = package_segments.next() else {
        return false;
    };
    let Some(name) = package_segments.next() else {
        return false;
    };
    if package_segments.next().is_some() || !scope.starts_with('@') {
        return false;
    }
    package_root.file_name() == Some(OsStr::new(name))
        && package_root.parent().and_then(Path::file_name) == Some(OsStr::new(scope))
        && package_root
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            == Some(OsStr::new("node_modules"))
}

fn valid_package_version(version: &str) -> bool {
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        || !version
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    let core_end = version.find(['-', '+']).unwrap_or(version.len());
    let mut core = version[..core_end].split('.');
    let parts = [core.next(), core.next(), core.next()];
    core.next().is_none()
        && parts.into_iter().all(|part| {
            part.is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

#[cfg(unix)]
fn find_executable_in(name: &str, home: &Path, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_candidates = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(name));
    let known_candidates = [
        home.join(".local/bin").join(name),
        home.join(".bun/bin").join(name),
        home.join("bin").join(name),
        home.join("Library/pnpm").join(name),
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
        PathBuf::from("/Applications/Codex.app/Contents/MacOS").join(name),
        home.join("Applications/Codex.app/Contents/MacOS")
            .join(name),
    ];
    path_candidates.chain(known_candidates).find(|candidate| {
        std::fs::metadata(candidate)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

#[cfg(unix)]
fn validate_executable(path: &Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "native renderer is unavailable at {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "native renderer is not an executable file: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_defaults_keep_authority_and_renderer_separate() {
        let options = Options::default()
            .resolve(
                Path::new("/Applications/Hyper Term.app/Contents/MacOS/hyper-term"),
                Path::new("/Users/example"),
            )
            .expect("resolve packaged paths");
        assert_eq!(
            options.ui,
            Path::new("/Applications/Hyper Term.app/Contents/MacOS/hyper-term-ui")
        );
        assert_eq!(
            options.terminal_assets,
            Path::new("/Applications/Hyper Term.app/Contents/Resources/terminal")
        );
        assert_eq!(options.workbench_assets, None);
        assert_eq!(options.shell_cwd, Path::new("/Users/example"));
    }

    #[test]
    fn arguments_support_explicit_development_artifacts() {
        let options = Options::parse([
            "--ui".into(),
            "/tmp/hyper-term-ui".into(),
            "--terminal-assets".into(),
            "/tmp/terminal".into(),
            "--workbench-assets".into(),
            "/tmp/workbench".into(),
            "--state-dir".into(),
            "/tmp/state".into(),
            "--shell-cwd".into(),
            "/tmp".into(),
            "--codex".into(),
            "/tmp/codex".into(),
            "--codex-auth".into(),
            "/tmp/auth.json".into(),
            "--codex-acp".into(),
            "/tmp/codex-acp".into(),
            "--claude-agent-acp".into(),
            "/tmp/claude-agent-acp".into(),
            "--claude".into(),
            "/tmp/claude".into(),
            "--copilot".into(),
            "/tmp/copilot".into(),
            "--deno-runtime".into(),
            "/tmp/deno".into(),
            "--genui-script".into(),
            "/tmp/genui.js".into(),
            "--genui-wasm".into(),
            "/tmp/esbuild.wasm".into(),
            "--genui-preview".into(),
            "/tmp/genui-preview.html".into(),
        ])
        .expect("options");
        assert_eq!(options.ui, Some(PathBuf::from("/tmp/hyper-term-ui")));
        assert_eq!(
            options.terminal_assets,
            Some(PathBuf::from("/tmp/terminal"))
        );
        assert_eq!(
            options.workbench_assets,
            Some(PathBuf::from("/tmp/workbench"))
        );
        assert_eq!(options.state_directory, Some(PathBuf::from("/tmp/state")));
        assert_eq!(options.shell_cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(options.codex, Some(PathBuf::from("/tmp/codex")));
        assert_eq!(options.codex_auth, Some(PathBuf::from("/tmp/auth.json")));
        assert_eq!(options.codex_acp, Some(PathBuf::from("/tmp/codex-acp")));
        assert_eq!(
            options.claude_agent_acp,
            Some(PathBuf::from("/tmp/claude-agent-acp"))
        );
        assert_eq!(options.claude, Some(PathBuf::from("/tmp/claude")));
        assert_eq!(options.copilot, Some(PathBuf::from("/tmp/copilot")));
        assert_eq!(options.deno_runtime, Some(PathBuf::from("/tmp/deno")));
        assert_eq!(options.genui_script, Some(PathBuf::from("/tmp/genui.js")));
        assert_eq!(options.genui_wasm, Some(PathBuf::from("/tmp/esbuild.wasm")));
        assert_eq!(
            options.genui_preview,
            Some(PathBuf::from("/tmp/genui-preview.html"))
        );
    }

    #[test]
    fn desktop_help_does_not_include_patch_markers() {
        assert!(
            !DESKTOP_HELP
                .lines()
                .any(|line| line.trim_start().starts_with('+'))
        );
        assert!(DESKTOP_HELP.contains("--workbench-assets PATH"));
    }

    #[test]
    fn desktop_tokens_are_url_safe_and_strong() {
        let token = desktop_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn desktop_shutdown_reaps_the_native_renderer() {
        let mut renderer = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("start renderer fixture");

        let status = wait_for_renderer_until(&mut renderer, async { Ok(()) })
            .await
            .expect("stop renderer");

        assert!(!status.success());
        assert!(renderer.try_wait().expect("inspect renderer").is_some());
    }

    #[test]
    fn codex_discovery_survives_a_finder_style_empty_path() {
        let temporary = tempfile::tempdir().expect("temporary home");
        let executable = temporary.path().join(".local/bin/codex");
        std::fs::create_dir_all(executable.parent().unwrap()).expect("binary directory");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("fake Codex");
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();

        assert_eq!(
            find_executable_in("codex", temporary.path(), None),
            Some(executable)
        );
    }

    #[test]
    fn provider_inventory_and_acp_environment_are_explicit() {
        let temporary = tempfile::tempdir().expect("temporary providers");
        let codex = temporary.path().join("codex");
        let claude = temporary.path().join("claude");
        let copilot = temporary.path().join("copilot");
        for (path, script) in [
            (&codex, "#!/bin/sh\n[ \"$1 $2\" = \"login status\" ]\n"),
            (
                &claude,
                "#!/bin/sh\n[ \"$1 $2\" = \"auth status\" ] && exit 1\nexit 2\n",
            ),
            (
                &copilot,
                "#!/bin/sh\n[ \"$1\" = \"--version\" ] && printf '%s\\n' 'GitHub Copilot CLI 1.0.69'\n",
            ),
        ] {
            std::fs::write(path, script).expect("fake provider");
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
        let resolved = ResolvedOptions {
            ui: "/tmp/ui".into(),
            terminal_assets: "/tmp/terminal".into(),
            workbench_assets: None,
            state_directory: "/tmp/state".into(),
            shell_cwd: "/tmp".into(),
            codex: Some(codex.clone()),
            codex_auth: None,
            codex_acp: Some(ResolvedAcpAdapter::installed(
                "/opt/homebrew/bin/codex-acp".into(),
            )),
            claude_agent_acp: Some(ResolvedAcpAdapter::installed(
                "/opt/homebrew/bin/claude-agent-acp".into(),
            )),
            claude: Some(claude.clone()),
            copilot: Some(copilot.clone()),
            mcp: None,
            genui_runtime: None,
        };
        assert_eq!(
            resolved
                .agent_provider_inventory(temporary.path())
                .expect("provider inventory")
                .into_iter()
                .map(|provider| (provider.id, provider.readiness))
                .collect::<Vec<_>>(),
            vec![
                ("codex".into(), AgentProviderReadiness::Authenticated),
                ("codex-acp".into(), AgentProviderReadiness::Authenticated),
                ("claude-acp".into(), AgentProviderReadiness::LoginRequired),
                ("copilot-acp".into(), AgentProviderReadiness::Available),
            ]
        );

        let environment = acp_environment(
            Path::new("/Users/example"),
            Path::new("/opt/homebrew/bin/codex-acp"),
        )
        .expect("ACP environment");
        assert_eq!(
            environment.get("HOME"),
            Some(&OsString::from("/Users/example"))
        );
        let path = environment.get("PATH").expect("PATH");
        assert_eq!(
            std::env::split_paths(path).next(),
            Some(PathBuf::from("/opt/homebrew/bin"))
        );
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
        assert!(!environment.contains_key("OPENAI_API_KEY"));
        let providers = resolved
            .acp_providers(Path::new("/Users/example"))
            .expect("ACP providers");
        assert_eq!(
            providers[1].environment.get("CLAUDE_CODE_EXECUTABLE"),
            Some(&claude.as_os_str().to_owned())
        );
        assert_eq!(providers[2].provider_id, "copilot-acp");
        assert_eq!(
            providers[2].arguments,
            [
                "--acp",
                "--stdio",
                "--no-auto-update",
                "--no-remote",
                "--no-remote-export",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn provider_probe_times_out_and_kills_a_stuck_process() {
        let temporary = tempfile::tempdir().expect("temporary provider");
        let executable = temporary.path().join("stuck-provider");
        std::fs::write(&executable, "#!/bin/sh\nsleep 5\n").expect("fake provider");
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();

        let started = Instant::now();
        assert_eq!(
            run_bounded_probe(&executable, &["--version"], None, Duration::from_millis(50))
                .expect("probe"),
            ProbeOutcome::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn automatic_codex_acp_discovery_accepts_only_known_npm_entrypoints() {
        let temporary = tempfile::tempdir().expect("temporary adapters");
        for (package, version, entrypoint) in [
            ("@zed-industries/codex-acp", "0.15.0", "bin/codex-acp.js"),
            ("@agentclientprotocol/codex-acp", "1.1.4", "dist/index.js"),
        ] {
            let package_root = temporary.path().join("node_modules").join(package);
            let executable = package_root.join(entrypoint);
            std::fs::create_dir_all(executable.parent().unwrap()).expect("adapter package");
            std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("adapter entrypoint");
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).unwrap();
            std::fs::write(
                package_root.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": package,
                    "version": version,
                    "bin": { "codex-acp": entrypoint }
                }))
                .unwrap(),
            )
            .expect("adapter manifest");

            let resolved = resolve_known_acp_adapter(executable, "codex-acp")
                .expect("known Codex ACP package");
            assert_eq!(
                resolved.implementation_version,
                format!("{package}@{version}")
            );
        }

        let spoofed = temporary.path().join("spoofed-codex-acp");
        std::fs::write(
            &spoofed,
            "#!/bin/sh\nprintf '%s\\n' '@agentclientprotocol/codex-acp 1.1.4'\n",
        )
        .expect("spoofed adapter");
        let mut permissions = std::fs::metadata(&spoofed).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&spoofed, permissions).unwrap();
        assert!(resolve_known_acp_adapter(spoofed, "codex-acp").is_none());

        let pretender_root = temporary.path().join("pretender");
        let pretender = pretender_root.join("codex-acp.js");
        std::fs::create_dir_all(&pretender_root).expect("pretender package");
        std::fs::write(&pretender, "#!/bin/sh\nexit 0\n").expect("pretender entrypoint");
        let mut permissions = std::fs::metadata(&pretender).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&pretender, permissions).unwrap();
        std::fs::write(
            pretender_root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "@zed-industries/codex-acp",
                "version": "0.15.0",
                "bin": { "codex-acp": "codex-acp.js" }
            }))
            .unwrap(),
        )
        .expect("pretender manifest");
        assert!(resolve_known_acp_adapter(pretender, "codex-acp").is_none());
    }

    #[test]
    fn automatic_claude_acp_discovery_requires_the_known_npm_entrypoint() {
        let temporary = tempfile::tempdir().expect("temporary adapter");
        let package_root = temporary
            .path()
            .join("node_modules/@agentclientprotocol/claude-agent-acp");
        let executable = package_root.join("dist/index.js");
        std::fs::create_dir_all(executable.parent().unwrap()).expect("adapter package");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("adapter entrypoint");
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        std::fs::write(
            package_root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "@agentclientprotocol/claude-agent-acp",
                "version": "0.59.0",
                "bin": { "claude-agent-acp": "dist/index.js" }
            }))
            .unwrap(),
        )
        .expect("adapter manifest");

        let resolved = resolve_known_acp_adapter(executable, "claude-agent-acp")
            .expect("known Claude ACP package");
        assert_eq!(
            resolved.implementation_version,
            "@agentclientprotocol/claude-agent-acp@0.59.0"
        );
    }

    #[test]
    fn automatic_codex_acp_discovery_rejects_manifest_bin_mismatch() {
        let temporary = tempfile::tempdir().expect("temporary adapter");
        let package_root = temporary
            .path()
            .join("node_modules/@zed-industries/codex-acp");
        let executable = package_root.join("bin/codex-acp.js");
        let other = package_root.join("bin/other.js");
        std::fs::create_dir_all(executable.parent().unwrap()).expect("adapter package");
        for path in [&executable, &other] {
            std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("adapter entrypoint");
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
        std::fs::write(
            package_root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "@zed-industries/codex-acp",
                "version": "0.15.0",
                "bin": { "codex-acp": "bin/other.js" }
            }))
            .unwrap(),
        )
        .expect("adapter manifest");
        assert!(resolve_known_acp_adapter(executable, "codex-acp").is_none());
    }

    #[test]
    fn automatic_acp_discovery_accepts_only_bounded_semver_versions() {
        for version in ["0.15.0", "1.2.3-beta.1", "2.0.0+darwin-arm64"] {
            assert!(valid_package_version(version), "{version}");
        }
        for version in ["", "1", "1.2", "1.2.3.4", "1..3", "1.2.3-"] {
            assert!(!valid_package_version(version), "{version}");
        }
        assert!(!valid_package_version(&"1".repeat(129)));
    }

    #[test]
    #[ignore = "requires HYPER_TERM_ACP_PATH to select an installed known Codex ACP package"]
    fn installed_codex_acp_is_bound_to_its_npm_package() {
        let executable = std::env::var_os("HYPER_TERM_ACP_PATH")
            .map(PathBuf::from)
            .expect("HYPER_TERM_ACP_PATH");
        let resolved = resolve_known_acp_adapter(executable, "codex-acp")
            .expect("known installed Codex ACP package");
        assert!(
            resolved
                .implementation_version
                .starts_with("@zed-industries/codex-acp@")
                || resolved
                    .implementation_version
                    .starts_with("@agentclientprotocol/codex-acp@")
        );
    }

    #[test]
    fn bundled_acp_runtime_is_digest_verified_and_launched_by_deno() {
        let (_temporary, runtime, deno) = bundled_acp_fixture();
        let adapters = load_bundled_acp_runtime(&runtime, &deno).expect("bundled adapters");
        assert_eq!(
            adapters.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["claude-acp", "codex-acp"]
        );
        let codex = &adapters["codex-acp"];
        assert_eq!(codex.executable, deno);
        assert_eq!(codex.implementation_version, "1.1.4");
        assert_eq!(codex.arguments[0], "run");
        assert_eq!(codex.arguments[1], "--cached-only");
        assert!(
            Path::new(codex.arguments.last().expect("entrypoint"))
                .ends_with("node_modules/@agentclientprotocol/codex-acp/dist/index.js")
        );
    }

    #[test]
    fn bundled_acp_runtime_rejects_a_tampered_dependency() {
        let (_temporary, runtime, deno) = bundled_acp_fixture();
        std::fs::write(
            runtime.join("acp/node_modules/@agentclientprotocol/codex-acp/dist/index.js"),
            "tampered",
        )
        .expect("tamper adapter");
        let error = load_bundled_acp_runtime(&runtime, &deno)
            .expect_err("tampered adapter must fail closed");
        assert!(error.contains("size changed") || error.contains("digest changed"));
    }

    fn bundled_acp_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temporary = tempfile::tempdir().expect("temporary bundle");
        let runtime = temporary.path().join("Contents/Resources/runtime");
        let acp = runtime.join("acp");
        let deno = runtime.join("deno");
        std::fs::create_dir_all(acp.join("node_modules/@agentclientprotocol/codex-acp/dist"))
            .expect("Codex adapter directory");
        std::fs::create_dir_all(
            acp.join("node_modules/@agentclientprotocol/claude-agent-acp/dist"),
        )
        .expect("Claude adapter directory");
        std::fs::write(&deno, "#!/bin/sh\nexit 0\n").expect("fake Deno");
        let mut permissions = std::fs::metadata(&deno).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&deno, permissions).unwrap();

        let files = [
            (
                "node_modules/@agentclientprotocol/codex-acp/dist/index.js",
                "codex adapter",
            ),
            (
                "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js",
                "claude adapter",
            ),
            ("node_modules/empty-package-marker", ""),
        ];
        let inventory = files
            .iter()
            .map(|(relative, contents)| {
                let path = acp.join(relative);
                std::fs::write(&path, contents).expect("adapter fixture");
                serde_json::json!({
                    "path": relative,
                    "bytes": contents.len(),
                    "sha256": sha256_file(&path).expect("fixture digest"),
                })
            })
            .collect::<Vec<_>>();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "runtime": { "name": "deno", "version": "2.9.3" },
            "adapters": [
                {
                    "provider_id": "codex-acp",
                    "package": "@agentclientprotocol/codex-acp",
                    "version": "1.1.4",
                    "entrypoint": files[0].0,
                    "required_agent": "codex",
                    "entrypoint_sha256": inventory[0]["sha256"],
                },
                {
                    "provider_id": "claude-acp",
                    "package": "@agentclientprotocol/claude-agent-acp",
                    "version": "0.59.0",
                    "entrypoint": files[1].0,
                    "required_agent": "claude",
                    "entrypoint_sha256": inventory[1]["sha256"],
                },
            ],
            "files": inventory,
        });
        std::fs::write(
            acp.join("manifest.json"),
            serde_json::to_vec(&manifest).expect("manifest JSON"),
        )
        .expect("manifest");
        (temporary, runtime, deno)
    }
}
