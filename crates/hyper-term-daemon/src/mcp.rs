use std::path::PathBuf;

use hyper_term_daemon::{DenoMcpExecutorConfig, McpStdioConfig, run_mcp_stdio};

fn main() {
    if let Err(error) = run() {
        eprintln!("hyper-term-mcp: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = Options::parse(std::env::args().skip(1))?;
    if options.help {
        println!(
            "Hyper Term brokered MCP connector\n\n\
             Usage: hyper-term-mcp --agent-mode --socket PATH\n\n\
             Options:\n  --agent-mode   Required capability fence\n  \
             --socket PATH  hyperd Unix control socket\n  \
             --deno PATH    Enable Deno LSP with this pinned executable\n  \
             --deno-sha256 DIGEST  Expected Deno executable digest\n  \
             --workspace-snapshot PATH  Authority-created read snapshot\n  \
             --deno-cache PATH    Private prewarmed Deno cache\n  \
             --deno-scratch PATH  Private Deno scratch directory\n  \
             -h, --help     Show this help"
        );
        return Ok(());
    }
    let socket = options
        .socket
        .ok_or_else(|| "--socket is required".to_owned())?;
    let mut config =
        McpStdioConfig::new(socket, options.agent_mode).map_err(|error| error.to_string())?;
    let deno_values = [
        options.deno.is_some(),
        options.deno_sha256.is_some(),
        options.workspace_snapshot.is_some(),
        options.deno_cache.is_some(),
        options.deno_scratch.is_some(),
    ];
    if deno_values.iter().any(|value| *value) {
        if !deno_values.iter().all(|value| *value) {
            return Err("Deno LSP requires --deno, --deno-sha256, --workspace-snapshot, --deno-cache, and --deno-scratch".into());
        }
        config = config
            .with_deno_lsp(DenoMcpExecutorConfig {
                executable: options.deno.expect("checked"),
                executable_sha256: options.deno_sha256.expect("checked"),
                runtime_version: options.deno_version,
                workspace_snapshot: options.workspace_snapshot.expect("checked"),
                cache_directory: options.deno_cache.expect("checked"),
                scratch_directory: options.deno_scratch.expect("checked"),
            })
            .map_err(|error| error.to_string())?;
    }
    run_mcp_stdio(config, std::io::stdin(), &mut std::io::stdout())
        .map_err(|error| error.to_string())
}

struct Options {
    socket: Option<PathBuf>,
    agent_mode: bool,
    deno: Option<PathBuf>,
    deno_sha256: Option<String>,
    deno_version: String,
    workspace_snapshot: Option<PathBuf>,
    deno_cache: Option<PathBuf>,
    deno_scratch: Option<PathBuf>,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            socket: None,
            agent_mode: false,
            deno: None,
            deno_sha256: None,
            deno_version: "2.9.3".into(),
            workspace_snapshot: None,
            deno_cache: None,
            deno_scratch: None,
            help: false,
        }
    }
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--socket" => {
                    options.socket = Some(PathBuf::from(
                        arguments
                            .next()
                            .ok_or_else(|| "--socket requires a path".to_owned())?,
                    ));
                }
                "--agent-mode" => options.agent_mode = true,
                "--deno" => {
                    options.deno = Some(PathBuf::from(required(&mut arguments, "--deno")?));
                }
                "--deno-sha256" => {
                    options.deno_sha256 = Some(required(&mut arguments, "--deno-sha256")?);
                }
                "--deno-version" => {
                    options.deno_version = required(&mut arguments, "--deno-version")?;
                }
                "--workspace-snapshot" => {
                    options.workspace_snapshot = Some(PathBuf::from(required(
                        &mut arguments,
                        "--workspace-snapshot",
                    )?));
                }
                "--deno-cache" => {
                    options.deno_cache =
                        Some(PathBuf::from(required(&mut arguments, "--deno-cache")?));
                }
                "--deno-scratch" => {
                    options.deno_scratch =
                        Some(PathBuf::from(required(&mut arguments, "--deno-scratch")?));
                }
                "-h" | "--help" => options.help = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(options)
    }
}

fn required(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mode_is_an_explicit_capability_fence() {
        let options = Options::parse([
            "--agent-mode".into(),
            "--socket".into(),
            "/tmp/hyperd.sock".into(),
        ])
        .unwrap();
        assert!(options.agent_mode);
        assert_eq!(options.socket, Some(PathBuf::from("/tmp/hyperd.sock")));
    }
}
