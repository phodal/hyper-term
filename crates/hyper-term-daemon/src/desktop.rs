//! Rust-owned desktop supervisor for the packaged Native SDK renderer.
//!
//! The supervisor is the `.app` entry point. It owns daemon lifetime, the
//! authenticated loopback gateway, state paths, and the native renderer child.
//! The renderer still never spawns shells or receives a privileged bridge.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
use hyper_term_daemon::{
    AcpAgentProviderConfig, AgentGatewayConfig, AgentGenUiRuntimeConfig, DaemonState,
    TerminalGatewayConfig, spawn_agent_gateway, spawn_terminal_gateway, spawn_unix_server,
};
use uuid::Uuid;

const DESKTOP_TERMINAL_ADDRESS: &str = "127.0.0.1:47437";
const TERMINAL_URL_ENV: &str = "HYPER_TERM_TERMINAL_URL";
const AGENT_URL_ENV: &str = "HYPER_TERM_AGENT_URL";
const AGENT_PROVIDERS_ENV: &str = "HYPER_TERM_AGENT_PROVIDERS";

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
        println!(
            "Hyper Term desktop host\n\nUsage: hyper-term-desktop [OPTIONS]\n\n\
             Options:\n  --ui PATH                 Native renderer executable\n  \
             --terminal-assets PATH    Built terminal renderer directory\n  \
             --state-dir PATH          Durable Hyper Term state\n  \
             --shell-cwd PATH          Initial directory for new shells\n  \
             --codex PATH              Codex executable for Agent sessions\n  \
             --codex-auth PATH         Private Codex auth.json for isolated Agent sessions\n  \
             --codex-acp PATH          Codex ACP adapter executable\n  \
             --claude-agent-acp PATH   Claude Agent ACP adapter executable\n  \
             --deno-runtime PATH       Pinned Deno executable for brokered Agent tools\n  \
             --genui-script PATH       Bundled GenUI compiler service\n  \
             --genui-wasm PATH         Pinned esbuild-wasm compiler binary\n  \
             --genui-preview PATH      Bundled isolated GenUI preview capsule\n  \
             -h, --help                Show this help"
        );
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
        let acp_providers = resolved.acp_providers(&home)?;
        let agent_provider_ids = resolved.agent_provider_ids();
        let agent_gateway = spawn_agent_gateway(AgentGatewayConfig {
            bind: "127.0.0.1:0".parse().expect("agent loopback bind is valid"),
            token: agent_token.clone(),
            workspace: resolved.shell_cwd.clone(),
            state_directory: resolved.state_directory.join("agent-runtime"),
            daemon: daemon.clone(),
            codex_executable: resolved.codex.clone(),
            codex_auth_file: resolved.codex_auth.clone(),
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
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    loop {
        if let Some(status) = renderer
            .try_wait()
            .map_err(|error| format!("cannot inspect native renderer: {error}"))?
        {
            return Ok(status);
        }
        tokio::select! {
            result = &mut interrupt => {
                result.map_err(|error| format!("cannot wait for desktop interrupt: {error}"))?;
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
    state_directory: Option<PathBuf>,
    shell_cwd: Option<PathBuf>,
    codex: Option<PathBuf>,
    codex_auth: Option<PathBuf>,
    codex_acp: Option<PathBuf>,
    claude_agent_acp: Option<PathBuf>,
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
                deno_executable,
                compiler_script,
                compiler_wasm,
                preview_shell,
            });
        Ok(ResolvedOptions {
            ui: self
                .ui
                .unwrap_or_else(|| executable.with_file_name("hyper-term-ui")),
            terminal_assets: self
                .terminal_assets
                .unwrap_or_else(|| contents.join("Resources/terminal")),
            state_directory: self
                .state_directory
                .unwrap_or_else(|| default_state_directory(home)),
            shell_cwd: self.shell_cwd.unwrap_or_else(|| home.to_owned()),
            codex: self
                .codex
                .or_else(|| std::env::var_os("HYPER_TERM_CODEX_PATH").map(PathBuf::from))
                .or_else(|| find_executable("codex", home)),
            codex_auth: self.codex_auth.or_else(|| {
                let candidate = home.join(".codex/auth.json");
                candidate.is_file().then_some(candidate)
            }),
            codex_acp: self
                .codex_acp
                .or_else(|| std::env::var_os("HYPER_TERM_CODEX_ACP_PATH").map(PathBuf::from))
                .or_else(|| find_executable("codex-acp", home)),
            claude_agent_acp: self
                .claude_agent_acp
                .or_else(|| std::env::var_os("HYPER_TERM_CLAUDE_AGENT_ACP_PATH").map(PathBuf::from))
                .or_else(|| find_executable("claude-agent-acp", home)),
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
    state_directory: PathBuf,
    shell_cwd: PathBuf,
    codex: Option<PathBuf>,
    codex_auth: Option<PathBuf>,
    codex_acp: Option<PathBuf>,
    claude_agent_acp: Option<PathBuf>,
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

impl ResolvedOptions {
    fn validate(&self) -> Result<(), String> {
        validate_executable(&self.ui)?;
        if !self.terminal_assets.join("index.html").is_file() {
            return Err(format!(
                "terminal assets are missing index.html: {}",
                self.terminal_assets.display()
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
            validate_executable(codex_acp)?;
        }
        if let Some(claude_agent_acp) = &self.claude_agent_acp {
            validate_executable(claude_agent_acp)?;
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

    fn agent_provider_ids(&self) -> String {
        let mut providers = Vec::with_capacity(3);
        if self.codex.is_some() {
            providers.push("codex");
        }
        if self.codex_acp.is_some() {
            providers.push("codex-acp");
        }
        if self.claude_agent_acp.is_some() {
            providers.push("claude-acp");
        }
        providers.join(",")
    }

    fn acp_providers(&self, home: &Path) -> Result<Vec<AcpAgentProviderConfig>, String> {
        let mut providers = Vec::with_capacity(2);
        if let Some(executable) = &self.codex_acp {
            let mut environment = acp_environment(home, executable)?;
            environment.insert("NO_BROWSER".into(), "1".into());
            if let Some(codex) = &self.codex {
                environment.insert("CODEX_PATH".into(), codex.as_os_str().to_owned());
            }
            providers.push(AcpAgentProviderConfig {
                provider_id: "codex-acp".into(),
                executable: executable.clone(),
                arguments: Vec::new(),
                environment,
                implementation_version: "installed".into(),
            });
        }
        if let Some(executable) = &self.claude_agent_acp {
            providers.push(AcpAgentProviderConfig {
                provider_id: "claude-acp".into(),
                executable: executable.clone(),
                arguments: Vec::new(),
                environment: acp_environment(home, executable)?,
                implementation_version: "installed".into(),
            });
        }
        Ok(providers)
    }
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
    ]))
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
        assert_eq!(options.shell_cwd, Path::new("/Users/example"));
    }

    #[test]
    fn arguments_support_explicit_development_artifacts() {
        let options = Options::parse([
            "--ui".into(),
            "/tmp/hyper-term-ui".into(),
            "--terminal-assets".into(),
            "/tmp/terminal".into(),
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
        assert_eq!(options.state_directory, Some(PathBuf::from("/tmp/state")));
        assert_eq!(options.shell_cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(options.codex, Some(PathBuf::from("/tmp/codex")));
        assert_eq!(options.codex_auth, Some(PathBuf::from("/tmp/auth.json")));
        assert_eq!(options.codex_acp, Some(PathBuf::from("/tmp/codex-acp")));
        assert_eq!(
            options.claude_agent_acp,
            Some(PathBuf::from("/tmp/claude-agent-acp"))
        );
        assert_eq!(options.deno_runtime, Some(PathBuf::from("/tmp/deno")));
        assert_eq!(options.genui_script, Some(PathBuf::from("/tmp/genui.js")));
        assert_eq!(options.genui_wasm, Some(PathBuf::from("/tmp/esbuild.wasm")));
        assert_eq!(
            options.genui_preview,
            Some(PathBuf::from("/tmp/genui-preview.html"))
        );
    }

    #[test]
    fn desktop_tokens_are_url_safe_and_strong() {
        let token = desktop_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
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
        let resolved = ResolvedOptions {
            ui: "/tmp/ui".into(),
            terminal_assets: "/tmp/terminal".into(),
            state_directory: "/tmp/state".into(),
            shell_cwd: "/tmp".into(),
            codex: Some("/opt/homebrew/bin/codex".into()),
            codex_auth: None,
            codex_acp: Some("/opt/homebrew/bin/codex-acp".into()),
            claude_agent_acp: None,
            mcp: None,
            genui_runtime: None,
        };
        assert_eq!(resolved.agent_provider_ids(), "codex,codex-acp");

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
    }
}
