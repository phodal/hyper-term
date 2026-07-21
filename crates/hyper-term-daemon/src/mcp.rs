use std::path::PathBuf;

use hyper_term_daemon::{McpStdioConfig, run_mcp_stdio};
use hyper_term_protocol::TaskId;
use uuid::Uuid;

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
             Usage: hyper-term-mcp --agent-mode --socket PATH [--task-id UUID]\n\n\
             Options:\n  --agent-mode   Required capability fence\n  \
             --socket PATH  hyperd Unix control socket\n  \
             --task-id UUID  Bind tool operations to an existing Agent task\n  \
             --enable-deno-lsp  Advertise the Rust-hosted Deno LSP tool\n  \
             --enable-genui  Advertise the Rust-hosted GenUI compiler\n  \
             -h, --help     Show this help"
        );
        return Ok(());
    }
    let socket = options
        .socket
        .ok_or_else(|| "--socket is required".to_owned())?;
    let mut config =
        McpStdioConfig::new(socket, options.agent_mode).map_err(|error| error.to_string())?;
    if let Some(task_id) = options.task_id {
        config = config.with_task(task_id);
    }
    if options.enable_deno_lsp {
        config = config.with_deno_lsp_enabled();
    }
    if options.enable_genui {
        config = config.with_deno_genui_enabled();
    }
    run_mcp_stdio(config, std::io::stdin(), &mut std::io::stdout())
        .map_err(|error| error.to_string())
}

#[derive(Default)]
struct Options {
    socket: Option<PathBuf>,
    task_id: Option<TaskId>,
    agent_mode: bool,
    enable_deno_lsp: bool,
    enable_genui: bool,
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
                "--task-id" => {
                    let value = required(&mut arguments, "--task-id")?;
                    let value = Uuid::parse_str(&value)
                        .map_err(|_| "--task-id requires a UUID".to_owned())?;
                    options.task_id = Some(TaskId::from_uuid(value));
                }
                "--agent-mode" => options.agent_mode = true,
                "--enable-deno-lsp" => options.enable_deno_lsp = true,
                "--enable-genui" => options.enable_genui = true,
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

    #[test]
    fn task_id_is_parsed_as_a_typed_agent_task() {
        let id = Uuid::new_v4();
        let options = Options::parse([
            "--agent-mode".into(),
            "--socket".into(),
            "/tmp/hyperd.sock".into(),
            "--task-id".into(),
            id.to_string(),
        ])
        .unwrap();
        assert_eq!(options.task_id, Some(TaskId::from_uuid(id)));
    }

    #[test]
    fn brokered_tool_inventory_is_enabled_without_runtime_paths() {
        let options = Options::parse([
            "--agent-mode".into(),
            "--socket".into(),
            "/tmp/hyperd.sock".into(),
            "--enable-genui".into(),
        ])
        .unwrap();
        assert!(options.enable_genui);
        assert!(!options.enable_deno_lsp);
    }
}
