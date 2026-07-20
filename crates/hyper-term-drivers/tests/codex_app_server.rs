use std::path::PathBuf;
use std::time::Duration;

use hyper_term_drivers::{
    CodexAppServerClient, CodexAppServerConfig, DriverState, StructuredAgentClient,
};
use tempfile::TempDir;

#[test]
#[ignore = "requires HYPER_TERM_CODEX_PATH, HYPER_TERM_CODEX_SHA256, and HYPER_TERM_CODEX_AUTH_PATH"]
fn installed_codex_app_server_starts_an_authenticated_isolated_thread() {
    let codex = PathBuf::from(
        std::env::var_os("HYPER_TERM_CODEX_PATH")
            .expect("HYPER_TERM_CODEX_PATH must select the inspected Codex binary"),
    )
    .canonicalize()
    .unwrap();
    let digest = std::env::var("HYPER_TERM_CODEX_SHA256")
        .expect("HYPER_TERM_CODEX_SHA256 must identify that exact binary");
    let auth_file = PathBuf::from(
        std::env::var_os("HYPER_TERM_CODEX_AUTH_PATH")
            .expect("HYPER_TERM_CODEX_AUTH_PATH must select a private auth.json"),
    );
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
        auth_file: Some(auth_file),
        brokered_mcp_server: None,
        containment: None,
    })
    .unwrap();
    let thread_id = client.initialize_session(Duration::from_secs(10)).unwrap();
    assert!(!thread_id.is_empty());
    assert_eq!(client.state().unwrap(), DriverState::Ready);
    let capabilities = client.session_capabilities().unwrap();
    assert!(
        capabilities
            .config_options
            .iter()
            .any(|option| option.id == "model" && !option.choices.is_empty())
    );
    assert!(
        capabilities
            .config_options
            .iter()
            .any(|option| option.id == "reasoning_effort" && !option.choices.is_empty())
    );
    assert_eq!(client.close().unwrap(), DriverState::Closed);
}
