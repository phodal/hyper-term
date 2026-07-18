use std::path::PathBuf;
use std::time::Duration;

use hyper_term_drivers::{CodexAppServerClient, CodexAppServerConfig, DriverState};
use tempfile::TempDir;

#[test]
#[ignore = "requires HYPER_TERM_CODEX_PATH and HYPER_TERM_CODEX_SHA256"]
fn installed_codex_app_server_completes_an_isolated_initialize() {
    let codex = PathBuf::from(
        std::env::var_os("HYPER_TERM_CODEX_PATH")
            .expect("HYPER_TERM_CODEX_PATH must select the inspected Codex binary"),
    )
    .canonicalize()
    .unwrap();
    let digest = std::env::var("HYPER_TERM_CODEX_SHA256")
        .expect("HYPER_TERM_CODEX_SHA256 must identify that exact binary");
    let workspace = TempDir::new().unwrap();
    let codex_home = TempDir::new().unwrap();
    let scratch = TempDir::new().unwrap();
    let client = CodexAppServerClient::launch(CodexAppServerConfig {
        executable: codex,
        executable_sha256: digest,
        implementation_version: "0.144.5".into(),
        workspace: workspace.path().canonicalize().unwrap(),
        codex_home: codex_home.path().canonicalize().unwrap(),
        scratch_directory: scratch.path().canonicalize().unwrap(),
        brokered_mcp_server: None,
    })
    .unwrap();
    let response = client.initialize(Duration::from_secs(10)).unwrap();
    assert!(
        response["result"]["userAgent"]
            .as_str()
            .is_some_and(|value| value.contains("0.144.5")),
        "unexpected initialize response: {response}"
    );
    assert_eq!(response["result"]["platformFamily"], "unix");
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}
