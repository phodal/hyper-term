use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use hyper_term_drivers::{
    DenoGenUiCompiler, DenoGenUiConfig, DriverState, GenUiCompileRequest, sha256_file,
};
use tempfile::tempdir;

#[test]
#[ignore = "requires HYPER_TERM_DENO_PATH, HYPER_TERM_DENO_SHA256, and built GenUI runtime assets"]
fn pinned_deno_compiles_a_real_genui_artifact_without_workspace_authority() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .unwrap()
        .canonicalize()
        .unwrap();
    let executable =
        PathBuf::from(std::env::var_os("HYPER_TERM_DENO_PATH").expect("HYPER_TERM_DENO_PATH"))
            .canonicalize()
            .unwrap();
    let compiler_script = root.join("dist/runtime/genui-compiler.js");
    let compiler_wasm = root.join("dist/runtime/esbuild.wasm");
    let private = tempdir().unwrap();
    let cache = private.path().join("cache");
    let scratch = private.path().join("scratch");
    let compiler = DenoGenUiCompiler::launch(
        DenoGenUiConfig {
            executable,
            executable_sha256: std::env::var("HYPER_TERM_DENO_SHA256")
                .expect("HYPER_TERM_DENO_SHA256"),
            runtime_version: "2.9.3".into(),
            compiler_script_sha256: sha256_file(&compiler_script).unwrap(),
            compiler_script,
            compiler_wasm_sha256: sha256_file(&compiler_wasm).unwrap(),
            compiler_wasm,
            compiler_version: "0.28.1".into(),
            cache_directory: cache,
            scratch_directory: scratch,
        },
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(compiler.state().unwrap(), DriverState::Ready);

    let candidate = compiler
        .compile(
            GenUiCompileRequest {
                source_revision: 11,
                entrypoint: "/App.tsx".into(),
                files: BTreeMap::from([(
                    "/App.tsx".into(),
                    "export default function App() { return <main data-probe=\"rust\">Hello</main>; }"
                        .into(),
                )]),
            },
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(candidate.source_revision, 11);
    assert_eq!(candidate.compiler.version, "0.28.1");
    assert_eq!(candidate.content_digest.len(), 64);
    assert!(candidate.bundle.contains("data-probe"));
    assert_eq!(compiler.shutdown().unwrap(), DriverState::Closed);
}
