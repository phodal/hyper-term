use std::path::PathBuf;

use hyper_term_daemon::{McpStdioConfig, run_mcp_stdio};

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
             -h, --help     Show this help"
        );
        return Ok(());
    }
    let socket = options
        .socket
        .ok_or_else(|| "--socket is required".to_owned())?;
    let config =
        McpStdioConfig::new(socket, options.agent_mode).map_err(|error| error.to_string())?;
    run_mcp_stdio(config, std::io::stdin(), &mut std::io::stdout())
        .map_err(|error| error.to_string())
}

#[derive(Default)]
struct Options {
    socket: Option<PathBuf>,
    agent_mode: bool,
    help: bool,
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
                "-h" | "--help" => options.help = true,
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(options)
    }
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
