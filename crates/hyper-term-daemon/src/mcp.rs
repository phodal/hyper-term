use std::path::PathBuf;

use hyper_term_daemon::{
    DenoGenUiMcpExecutorConfig, DenoMcpExecutorConfig, McpStdioConfig, run_mcp_stdio,
};
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
             --deno PATH    Enable Deno LSP with this pinned executable\n  \
             --deno-sha256 DIGEST  Expected Deno executable digest\n  \
             --workspace-snapshot PATH  Authority-created read snapshot\n  \
             --deno-cache PATH    Private prewarmed Deno cache\n  \
             --deno-scratch PATH  Private Deno scratch directory\n  \
             --genui-script PATH  Enable GenUI with this bundled compiler service\n  \
             --genui-script-sha256 DIGEST  Expected compiler service digest\n  \
             --genui-wasm PATH    Pinned esbuild-wasm binary\n  \
             --genui-wasm-sha256 DIGEST  Expected esbuild-wasm digest\n  \
             --genui-compiler-version VERSION  Expected compiler handshake version\n  \
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
    let deno_common = [
        options.deno.is_some(),
        options.deno_sha256.is_some(),
        options.deno_cache.is_some(),
        options.deno_scratch.is_some(),
    ];
    let genui_values = [
        options.genui_script.is_some(),
        options.genui_script_sha256.is_some(),
        options.genui_wasm.is_some(),
        options.genui_wasm_sha256.is_some(),
    ];
    let lsp_requested = options.workspace_snapshot.is_some();
    let genui_requested = genui_values.iter().any(|value| *value);
    if (lsp_requested || genui_requested) && !deno_common.iter().all(|value| *value) {
        return Err(
            "Deno tools require --deno, --deno-sha256, --deno-cache, and --deno-scratch".into(),
        );
    }
    if deno_common.iter().any(|value| *value) && !(lsp_requested || genui_requested) {
        return Err("Deno runtime options require an LSP snapshot or GenUI compiler".into());
    }
    if genui_requested && !genui_values.iter().all(|value| *value) {
        return Err("GenUI requires --genui-script, --genui-script-sha256, --genui-wasm, and --genui-wasm-sha256".into());
    }
    if lsp_requested {
        config = config
            .with_deno_lsp(DenoMcpExecutorConfig {
                executable: options.deno.clone().expect("checked"),
                executable_sha256: options.deno_sha256.clone().expect("checked"),
                runtime_version: options.deno_version.clone(),
                workspace_snapshot: options.workspace_snapshot.expect("checked"),
                cache_directory: options.deno_cache.clone().expect("checked"),
                scratch_directory: options.deno_scratch.clone().expect("checked"),
            })
            .map_err(|error| error.to_string())?;
    }
    if genui_requested {
        config = config
            .with_deno_genui(DenoGenUiMcpExecutorConfig {
                executable: options.deno.expect("checked"),
                executable_sha256: options.deno_sha256.expect("checked"),
                runtime_version: options.deno_version,
                compiler_script: options.genui_script.expect("checked"),
                compiler_script_sha256: options.genui_script_sha256.expect("checked"),
                compiler_wasm: options.genui_wasm.expect("checked"),
                compiler_wasm_sha256: options.genui_wasm_sha256.expect("checked"),
                compiler_version: options.genui_compiler_version,
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
    task_id: Option<TaskId>,
    agent_mode: bool,
    deno: Option<PathBuf>,
    deno_sha256: Option<String>,
    deno_version: String,
    workspace_snapshot: Option<PathBuf>,
    deno_cache: Option<PathBuf>,
    deno_scratch: Option<PathBuf>,
    genui_script: Option<PathBuf>,
    genui_script_sha256: Option<String>,
    genui_wasm: Option<PathBuf>,
    genui_wasm_sha256: Option<String>,
    genui_compiler_version: String,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            socket: None,
            task_id: None,
            agent_mode: false,
            deno: None,
            deno_sha256: None,
            deno_version: "2.9.3".into(),
            workspace_snapshot: None,
            deno_cache: None,
            deno_scratch: None,
            genui_script: None,
            genui_script_sha256: None,
            genui_wasm: None,
            genui_wasm_sha256: None,
            genui_compiler_version: "0.28.1".into(),
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
                "--task-id" => {
                    let value = required(&mut arguments, "--task-id")?;
                    let value = Uuid::parse_str(&value)
                        .map_err(|_| "--task-id requires a UUID".to_owned())?;
                    options.task_id = Some(TaskId::from_uuid(value));
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
                "--genui-script" => {
                    options.genui_script =
                        Some(PathBuf::from(required(&mut arguments, "--genui-script")?));
                }
                "--genui-script-sha256" => {
                    options.genui_script_sha256 =
                        Some(required(&mut arguments, "--genui-script-sha256")?);
                }
                "--genui-wasm" => {
                    options.genui_wasm =
                        Some(PathBuf::from(required(&mut arguments, "--genui-wasm")?));
                }
                "--genui-wasm-sha256" => {
                    options.genui_wasm_sha256 =
                        Some(required(&mut arguments, "--genui-wasm-sha256")?);
                }
                "--genui-compiler-version" => {
                    options.genui_compiler_version =
                        required(&mut arguments, "--genui-compiler-version")?;
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
    fn genui_runtime_assets_are_parsed_without_enabling_lsp() {
        let options = Options::parse([
            "--agent-mode".into(),
            "--socket".into(),
            "/tmp/hyperd.sock".into(),
            "--deno".into(),
            "/tmp/deno".into(),
            "--deno-sha256".into(),
            "a".repeat(64),
            "--deno-cache".into(),
            "/tmp/cache".into(),
            "--deno-scratch".into(),
            "/tmp/scratch".into(),
            "--genui-script".into(),
            "/tmp/genui.js".into(),
            "--genui-script-sha256".into(),
            "b".repeat(64),
            "--genui-wasm".into(),
            "/tmp/esbuild.wasm".into(),
            "--genui-wasm-sha256".into(),
            "c".repeat(64),
        ])
        .unwrap();
        assert!(options.workspace_snapshot.is_none());
        assert_eq!(options.genui_compiler_version, "0.28.1");
        assert_eq!(options.genui_script, Some(PathBuf::from("/tmp/genui.js")));
    }
}
