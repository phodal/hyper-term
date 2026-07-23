use std::path::PathBuf;
use std::time::Duration;

use hyper_term_drivers::{DenoLspClient, DenoLspConfig, DriverState};
use serde_json::json;
use tempfile::TempDir;

#[test]
#[ignore = "requires HYPER_TERM_DENO_PATH and HYPER_TERM_DENO_SHA256"]
fn pinned_deno_lsp_completes_a_real_initialize_handshake() {
    let deno = PathBuf::from(
        std::env::var_os("HYPER_TERM_DENO_PATH")
            .expect("HYPER_TERM_DENO_PATH must select the verified runtime"),
    )
    .canonicalize()
    .unwrap();
    let digest = std::env::var("HYPER_TERM_DENO_SHA256")
        .expect("HYPER_TERM_DENO_SHA256 must come from the runtime manifest");
    let workspace = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let scratch = TempDir::new().unwrap();
    let client = DenoLspClient::launch(DenoLspConfig {
        executable: deno,
        executable_sha256: digest,
        runtime_version: "2.9.3".into(),
        workspace_snapshot: workspace.path().canonicalize().unwrap(),
        cache_directory: cache.path().canonicalize().unwrap(),
        scratch_directory: scratch.path().canonicalize().unwrap(),
        config_file: None,
    })
    .unwrap();
    let response = client.initialize(Duration::from_secs(10)).unwrap();
    assert!(response["result"]["capabilities"].is_object());
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    let source = "const answer: string = 42;\n";
    let source_path = workspace.path().join("main.ts");
    std::fs::write(&source_path, source).unwrap();
    let source_uri = format!("file://{}", source_path.display());
    client
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": source_uri,
                    "languageId": "typescript",
                    "version": 1,
                    "text": source
                }
            }),
        )
        .unwrap();
    let diagnostics = client
        .wait_for_notification("textDocument/publishDiagnostics", Duration::from_secs(10))
        .unwrap();
    assert_eq!(diagnostics["params"]["uri"], source_uri);
    assert!(
        diagnostics["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    let symbols = client
        .request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": source_uri}}),
            Duration::from_secs(10),
        )
        .unwrap();
    assert!(
        symbols["result"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert_eq!(
        client.shutdown(Duration::from_secs(2)).unwrap(),
        DriverState::Closed
    );
}
