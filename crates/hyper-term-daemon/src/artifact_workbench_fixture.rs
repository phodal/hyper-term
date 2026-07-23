use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hyper_term_daemon::{
    AcpAgentProviderConfig, AgentGatewayConfig, AgentGenUiRuntimeConfig, DaemonState,
    spawn_agent_gateway,
};
use hyper_term_protocol::{
    GenUiArtifactCandidate, GenUiCompilerIdentity, OperationAction, OperationKind,
    PermissionDecision, RiskClass, TaskId,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const SESSION_ID: u16 = 8;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let repository = std::env::current_dir()?;
    let fixture_root = required_path("HYPER_TERM_FIXTURE_ROOT")?;
    fs::create_dir_all(&fixture_root)?;
    let fixture_root = fixture_root.canonicalize()?;
    let workspace = create_directory(&fixture_root.join("workspace"))?;
    let provider_home = create_directory(&fixture_root.join("provider-home"))?;
    let daemon = DaemonState::open(fixture_root.join("daemon-state"))?;
    let fake_acp = fixture_acp(&provider_home)?;

    let deno = configured_path(
        "HYPER_TERM_DENO_PATH",
        &repository.join(".tools/deno/2.9.3/deno"),
    )?;
    verify_runtime_digest(&deno)?;
    let runtime_root = configured_path(
        "HYPER_TERM_GENUI_RUNTIME_ROOT",
        &repository.join("dist/runtime"),
    )?;
    let workbench_assets = configured_path(
        "HYPER_TERM_WORKBENCH_ASSETS",
        &repository.join("dist/workbench"),
    )?;

    let gateway = spawn_agent_gateway(AgentGatewayConfig {
        bind: "127.0.0.1:0".parse()?,
        token: TOKEN.into(),
        workspace,
        state_directory: fixture_root.join("gateway-state"),
        daemon: daemon.clone(),
        provider_home: provider_home.clone(),
        codex_executable: None,
        codex_auth_file: None,
        acp_providers: vec![AcpAgentProviderConfig {
            provider_id: "fixture-acp".into(),
            executable: fake_acp,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            implementation_version: "artifact-workbench-fixture-1".into(),
        }],
        local_mcp_servers: Vec::new(),
        mcp_executable: None,
        genui_runtime: Some(AgentGenUiRuntimeConfig {
            deno_executable: deno,
            runtime_version: "2.9.3".into(),
            compiler_script: runtime_root.join("genui-compiler.js"),
            compiler_wasm: runtime_root.join("esbuild.wasm"),
            preview_shell: runtime_root.join("genui/preview.html"),
            compiler_version: "0.28.1".into(),
        }),
        workbench_assets: Some(workbench_assets),
        debug_capsule: None,
        tier2_runner: None,
        control_socket: provider_home.join("hyperd.sock"),
    })
    .await?;

    let session_path =
        format!("/agent/session?token={TOKEN}&session_id={SESSION_ID}&provider=fixture-acp");
    let session = request_json(gateway.address(), "POST", &session_path, b"").await?;
    let task_id: TaskId = serde_json::from_value(session["task_id"].clone())?;
    let proposed = daemon.propose_operation(
        task_id,
        OperationKind::McpTool,
        OperationAction::Opaque {
            kind: "hyper_term.genui.compile".into(),
            payload_digest: "a".repeat(64),
        },
        "Create the authenticated Artifact Workbench fixture".into(),
        RiskClass::ReadOnly,
        vec!["genui_compile".into()],
    )?;
    let authorized = daemon.decide_permission(
        task_id,
        proposed.operation_id,
        proposed.revision,
        PermissionDecision::AllowOnce,
    )?;
    let dispatching =
        daemon.begin_operation(task_id, proposed.operation_id, authorized.revision)?;

    let bundle = concat!(
        "globalThis.hyperTermArtifactWorkbenchFixture = true;",
        "globalThis.__HYPER_MOUNT__(function Fixture(){",
        "const h=globalThis.__HYPER_REACT__.createElement;",
        "return h('main',null,",
        "h('button',{type:'button','data-primary-action':'true'},'Run fixture'),",
        "h('p',null,'Artifact quality summary'));",
        "});"
    );
    let css = concat!(
        "#root{box-sizing:border-box;padding:24px;}",
        "main{display:flex;flex-direction:column;align-items:flex-start;gap:16px;",
        "max-width:100%;}",
        "button{min-width:120px;min-height:32px;border:1px solid #3f4b38;",
        "border-radius:6px;color:#f4f7ef;background:#1f291b;max-width:100%;",
        "white-space:normal;overflow-wrap:anywhere;}",
        "p{max-width:min(100%,640px);margin:0;line-height:1.55;overflow-wrap:anywhere;}",
        "button:focus-visible{outline:3px solid #84cc16;outline-offset:3px;}"
    );
    let accepted = daemon.accept_genui_artifact(
        task_id,
        proposed.operation_id,
        dispatching.revision,
        GenUiArtifactCandidate {
            schema_version: 1,
            source_revision: 1,
            entrypoint: "/main.ts".into(),
            source_files: BTreeMap::from([(
                "/main.ts".into(),
                concat!(
                    "const value = \"ok\";\n",
                    "value.toUpperCase();\n",
                    "export default value;\n"
                )
                .into(),
            )]),
            bundle: bundle.into(),
            css: css.into(),
            source_map: "{\"version\":3,\"sources\":[],\"names\":[],\"mappings\":\"\"}".into(),
            content_digest: sha256(&format!("{bundle}{css}")),
            compiler: GenUiCompilerIdentity {
                name: "esbuild-wasm".into(),
                version: "0.28.1".into(),
            },
            diagnostics: Vec::new(),
        },
    )?;

    let url = format!(
        "http://{}/agent/workbench/?token={TOKEN}&session_id={SESSION_ID}&surface=artifact&artifact_id={}",
        gateway.address(),
        accepted.artifact_id
    );
    println!("HYPER_TERM_ARTIFACT_WORKBENCH_URL={url}");
    println!("HYPER_TERM_ARTIFACT_ID={}", accepted.artifact_id);
    io::stdout().flush()?;

    tokio::signal::ctrl_c().await?;
    gateway.shutdown().await?;
    Ok(())
}

fn required_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| format!("{name} must be an absolute path").into())
}

fn configured_path(name: &str, fallback: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.to_owned());
    if !path.is_absolute() {
        return Err(format!("{name} must be an absolute path").into());
    }
    path.canonicalize()
        .map_err(|error| format!("{name} is unavailable at {}: {error}", path.display()).into())
}

fn create_directory(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    fs::create_dir_all(path)?;
    Ok(path.canonicalize()?)
}

fn fixture_acp(provider_home: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let executable = provider_home.join("fixture-acp");
    fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "while IFS= read -r line; do\n",
            "  case \"$line\" in\n",
            "    *'\"method\":\"initialize\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{},\"authMethods\":[],\"agentInfo\":{\"name\":\"fixture-acp\",\"version\":\"1\"}}}' ;;\n",
            "    *'\"method\":\"session/new\"'*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"artifact-workbench-session\"}}' ;;\n",
            "  esac\n",
            "done\n"
        ),
    )?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions)?;
    Ok(executable)
}

fn verify_runtime_digest(path: &Path) -> Result<(), Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let actual = sha256_bytes(&bytes);
    if let Some(expected) = std::env::var_os("HYPER_TERM_DENO_SHA256")
        && expected.to_string_lossy() != actual
    {
        return Err("Deno runtime digest does not match HYPER_TERM_DENO_SHA256".into());
    }
    Ok(())
}

fn sha256(value: &str) -> String {
    sha256_bytes(value.as_bytes())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn request_json(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<Value, Box<dyn Error>> {
    let mut stream = TcpStream::connect(address).await?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(body).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| io::Error::other("fixture gateway returned an invalid HTTP response"))?;
    let header = std::str::from_utf8(&response[..header_end])?;
    if !header.starts_with("HTTP/1.1 200 ") {
        return Err(format!(
            "fixture session request failed: {}",
            header.lines().next().unwrap_or(header)
        )
        .into());
    }
    Ok(serde_json::from_slice(&response[header_end..])?)
}
