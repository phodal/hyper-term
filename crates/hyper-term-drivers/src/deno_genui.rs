use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    DEFAULT_MAX_DRIVER_FRAME_BYTES, DriverError, DriverEvent, DriverFraming, DriverKind,
    DriverManifest, DriverProcess, DriverSpec, DriverState,
    deno_containment::compile_deno_task_sandbox, process::sandbox_permission_profile, sha256_file,
};

const GENUI_PROTOCOL_VERSION: u64 = 1;
const MAX_GENUI_FILES: usize = 100;
const MAX_GENUI_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_GENUI_BUNDLE_BYTES: usize = 768 * 1024;
const MAX_GENUI_CSS_BYTES: usize = 256 * 1024;
const MAX_GENUI_SOURCE_MAP_BYTES: usize = 768 * 1024;
const MAX_GENUI_DIAGNOSTICS: usize = 256;

#[derive(Clone, Debug)]
pub struct DenoGenUiConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub runtime_version: String,
    pub compiler_script: PathBuf,
    pub compiler_script_sha256: String,
    pub compiler_wasm: PathBuf,
    pub compiler_wasm_sha256: String,
    pub compiler_version: String,
    pub cache_directory: PathBuf,
    pub scratch_directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GenUiCompileRequest {
    pub source_revision: u64,
    pub entrypoint: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenUiCompilerIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenUiCompileDiagnostic {
    pub severity: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenUiArtifactCandidate {
    pub schema_version: u64,
    pub source_revision: u64,
    pub entrypoint: String,
    pub bundle: String,
    pub css: String,
    pub source_map: String,
    pub content_digest: String,
    pub compiler: GenUiCompilerIdentity,
    pub diagnostics: Vec<GenUiCompileDiagnostic>,
}

pub struct DenoGenUiCompiler {
    process: DriverProcess,
    request_gate: Mutex<()>,
    compiler_version: String,
}

impl DenoGenUiCompiler {
    pub fn launch(config: DenoGenUiConfig, timeout: Duration) -> Result<Self, DenoGenUiError> {
        validate_config(&config)?;
        fs::create_dir_all(&config.cache_directory)?;
        fs::create_dir_all(&config.scratch_directory)?;
        let compiler_script = config.compiler_script.canonicalize()?;
        let compiler_wasm = config.compiler_wasm.canonicalize()?;
        verify_asset(
            &compiler_script,
            &config.compiler_script_sha256,
            "compiler script",
        )?;
        verify_asset(
            &compiler_wasm,
            &config.compiler_wasm_sha256,
            "compiler WASM",
        )?;
        let cache = config.cache_directory.canonicalize()?;
        let scratch = config.scratch_directory.canonicalize()?;
        let read_allowlist = format!(
            "--allow-read={},{}",
            path_text(&compiler_script)?,
            path_text(&compiler_wasm)?
        );
        let environment = BTreeMap::from([
            ("DENO_DIR".into(), cache.clone().into_os_string()),
            ("DENO_NO_PROMPT".into(), OsString::from("1")),
            ("DENO_NO_UPDATE_CHECK".into(), OsString::from("1")),
            ("HOME".into(), scratch.clone().into_os_string()),
            ("NO_COLOR".into(), OsString::from("1")),
            ("TMPDIR".into(), scratch.clone().into_os_string()),
        ]);
        let arguments = vec![
            OsString::from("run"),
            OsString::from("--cached-only"),
            OsString::from("--no-config"),
            OsString::from("--no-lock"),
            OsString::from("--no-prompt"),
            OsString::from(read_allowlist),
            compiler_script.clone().into_os_string(),
            OsString::from("--wasm"),
            compiler_wasm.clone().into_os_string(),
        ];
        let driver_id = Uuid::new_v4();
        let sandbox = compile_deno_task_sandbox(
            driver_id,
            &config.executable,
            &arguments,
            &scratch,
            &environment,
            [compiler_script.clone(), compiler_wasm.clone()],
            [cache, scratch.clone()],
        )?;
        let permission_profile = sandbox_permission_profile(&sandbox);
        let process = DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id,
                kind: DriverKind::DenoGenUi,
                implementation_version: config.runtime_version,
                protocol_version: format!("hyper-term-genui-{GENUI_PROTOCOL_VERSION}"),
                capabilities: vec!["bounded_react_compile".into(), "source_maps".into()],
                transport: "stdio-json-lines".into(),
                executable_sha256: config.executable_sha256,
                permission_profile,
            },
            executable: config.executable,
            arguments,
            working_directory: scratch,
            environment,
            sandbox: Some(sandbox),
            framing: DriverFraming::JsonLines,
            max_frame_bytes: DEFAULT_MAX_DRIVER_FRAME_BYTES,
        })?;
        wait_until_ready(&process, &config.compiler_version, timeout)?;
        process.mark_ready()?;
        Ok(Self {
            process,
            request_gate: Mutex::new(()),
            compiler_version: config.compiler_version,
        })
    }

    pub fn compile(
        &self,
        request: GenUiCompileRequest,
        timeout: Duration,
    ) -> Result<GenUiArtifactCandidate, DenoGenUiError> {
        validate_request(&request)?;
        let _gate = lock(&self.request_gate)?;
        if self.process.state()? != DriverState::Ready {
            return Err(DenoGenUiError::NotReady);
        }
        let request_id = Uuid::new_v4().to_string();
        self.process.begin_effect()?;
        if let Err(error) = self.process.send_json(&json!({
            "type": "compile",
            "request_id": request_id,
            "source_revision": request.source_revision,
            "entrypoint": request.entrypoint,
            "files": request.files,
        })) {
            let _ = self.process.stop(Duration::from_millis(100));
            return Err(error.into());
        }
        let response = match self.wait_for_compile(&request_id, timeout) {
            Ok(response) => response,
            Err(error) => {
                let _ = self.process.stop(Duration::from_millis(100));
                return Err(error);
            }
        };
        match response {
            CompilerMessage::Compiled {
                request_id: response_id,
                source_revision,
                candidate,
            } => {
                if response_id != request_id || source_revision != request.source_revision {
                    return Err(self
                        .protocol_failure("compiler response does not match its request".into()));
                }
                if let Err(error) = validate_candidate(&candidate, &request, &self.compiler_version)
                {
                    let _ = self.process.stop(Duration::from_millis(100));
                    return Err(error);
                }
                self.process.finish_effect()?;
                Ok(candidate)
            }
            CompilerMessage::CompileFailed {
                request_id: response_id,
                source_revision,
                diagnostics,
            } => {
                if response_id != request_id || source_revision != request.source_revision {
                    return Err(
                        self.protocol_failure("compiler failure does not match its request".into())
                    );
                }
                if let Err(error) = validate_diagnostics(&diagnostics) {
                    let _ = self.process.stop(Duration::from_millis(100));
                    return Err(error);
                }
                self.process.finish_effect()?;
                Err(DenoGenUiError::CompileFailed(diagnostics))
            }
            CompilerMessage::ProtocolError { message } => Err(self.protocol_failure(message)),
            CompilerMessage::Ready { .. } => {
                Err(self
                    .protocol_failure("compiler emitted an unexpected lifecycle message".into()))
            }
        }
    }

    pub fn state(&self) -> Result<DriverState, DenoGenUiError> {
        Ok(self.process.state()?)
    }

    pub fn stderr_tail(&self) -> Result<String, DenoGenUiError> {
        Ok(self.process.stderr_tail()?)
    }

    pub fn shutdown(&self) -> Result<DriverState, DenoGenUiError> {
        Ok(self.process.stop(Duration::from_millis(250))?)
    }

    fn wait_for_compile(
        &self,
        request_id: &str,
        timeout: Duration,
    ) -> Result<CompilerMessage, DenoGenUiError> {
        match self.process.recv_timeout(timeout) {
            Ok(DriverEvent::Message { payload, .. }) => {
                serde_json::from_value(payload).map_err(|error| {
                    DenoGenUiError::Protocol(format!("invalid compiler response: {error}"))
                })
            }
            Ok(DriverEvent::ProtocolError { message }) => Err(DenoGenUiError::Protocol(message)),
            Ok(DriverEvent::Exited { state, .. }) => Err(DenoGenUiError::Exited {
                state,
                stderr: self.process.stderr_tail().unwrap_or_default(),
            }),
            Err(DriverError::Timeout | DriverError::EffectTimedOut { .. }) => {
                Err(DenoGenUiError::Timeout {
                    request_id: request_id.into(),
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    fn protocol_failure(&self, message: String) -> DenoGenUiError {
        let _ = self.process.stop(Duration::from_millis(100));
        DenoGenUiError::Protocol(message)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CompilerMessage {
    Ready {
        protocol_version: u64,
        compiler: GenUiCompilerIdentity,
    },
    Compiled {
        request_id: String,
        source_revision: u64,
        candidate: GenUiArtifactCandidate,
    },
    CompileFailed {
        request_id: String,
        source_revision: u64,
        diagnostics: Vec<GenUiCompileDiagnostic>,
    },
    ProtocolError {
        message: String,
    },
}

fn wait_until_ready(
    process: &DriverProcess,
    compiler_version: &str,
    timeout: Duration,
) -> Result<(), DenoGenUiError> {
    let event = process.recv_timeout(timeout)?;
    let DriverEvent::Message { payload, .. } = event else {
        return Err(match event {
            DriverEvent::ProtocolError { message } => DenoGenUiError::Protocol(message),
            DriverEvent::Exited { state, .. } => DenoGenUiError::Exited {
                state,
                stderr: process.stderr_tail().unwrap_or_default(),
            },
            DriverEvent::Message { .. } => unreachable!(),
        });
    };
    match serde_json::from_value(payload)
        .map_err(|error| DenoGenUiError::Protocol(format!("invalid ready message: {error}")))?
    {
        CompilerMessage::Ready {
            protocol_version,
            compiler,
        } if protocol_version == GENUI_PROTOCOL_VERSION
            && compiler.name == "esbuild-wasm"
            && compiler.version == compiler_version =>
        {
            Ok(())
        }
        CompilerMessage::Ready { .. } => Err(DenoGenUiError::Protocol(
            "compiler ready message has an unexpected protocol or version".into(),
        )),
        _ => Err(DenoGenUiError::Protocol(
            "compiler did not emit ready before other messages".into(),
        )),
    }
}

fn validate_config(config: &DenoGenUiConfig) -> Result<(), DenoGenUiError> {
    if !config.executable.is_absolute()
        || !config.compiler_script.is_absolute()
        || !config.compiler_wasm.is_absolute()
        || !config.cache_directory.is_absolute()
        || !config.scratch_directory.is_absolute()
    {
        return Err(DenoGenUiError::InvalidConfig(
            "Deno GenUI paths must be absolute".into(),
        ));
    }
    if config.runtime_version.is_empty()
        || config.compiler_version.is_empty()
        || !is_sha256(&config.executable_sha256)
        || !is_sha256(&config.compiler_script_sha256)
        || !is_sha256(&config.compiler_wasm_sha256)
    {
        return Err(DenoGenUiError::InvalidConfig(
            "Deno GenUI manifest is incomplete".into(),
        ));
    }
    Ok(())
}

fn validate_request(request: &GenUiCompileRequest) -> Result<(), DenoGenUiError> {
    if request.source_revision == 0 {
        return Err(DenoGenUiError::InvalidRequest(
            "source revision must be positive".into(),
        ));
    }
    if request.files.is_empty() || request.files.len() > MAX_GENUI_FILES {
        return Err(DenoGenUiError::InvalidRequest(format!(
            "source snapshot must contain 1-{MAX_GENUI_FILES} files"
        )));
    }
    if !request.files.contains_key(&request.entrypoint) {
        return Err(DenoGenUiError::InvalidRequest(
            "entrypoint is not present in the source snapshot".into(),
        ));
    }
    let mut source_bytes = 0usize;
    for (path, source) in &request.files {
        if !valid_virtual_path(path) {
            return Err(DenoGenUiError::InvalidRequest(format!(
                "invalid virtual source path: {path}"
            )));
        }
        source_bytes = source_bytes.saturating_add(source.len());
    }
    if source_bytes > MAX_GENUI_SOURCE_BYTES {
        return Err(DenoGenUiError::InvalidRequest(format!(
            "source snapshot exceeds {MAX_GENUI_SOURCE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_candidate(
    candidate: &GenUiArtifactCandidate,
    request: &GenUiCompileRequest,
    compiler_version: &str,
) -> Result<(), DenoGenUiError> {
    if candidate.schema_version != 1
        || candidate.source_revision != request.source_revision
        || candidate.entrypoint != request.entrypoint
        || candidate.compiler.name != "esbuild-wasm"
        || candidate.compiler.version != compiler_version
    {
        return Err(DenoGenUiError::Protocol(
            "artifact candidate metadata does not match the accepted request".into(),
        ));
    }
    if candidate.bundle.len() > MAX_GENUI_BUNDLE_BYTES
        || candidate.css.len() > MAX_GENUI_CSS_BYTES
        || candidate.source_map.len() > MAX_GENUI_SOURCE_MAP_BYTES
    {
        return Err(DenoGenUiError::Protocol(
            "artifact candidate exceeds its bounded output budget".into(),
        ));
    }
    validate_diagnostics(&candidate.diagnostics)?;
    let actual = sha256_text_pair(&candidate.bundle, &candidate.css);
    if candidate.content_digest != actual {
        return Err(DenoGenUiError::ArtifactDigestMismatch {
            expected: candidate.content_digest.clone(),
            actual,
        });
    }
    Ok(())
}

fn validate_diagnostics(diagnostics: &[GenUiCompileDiagnostic]) -> Result<(), DenoGenUiError> {
    if diagnostics.len() > MAX_GENUI_DIAGNOSTICS
        || diagnostics.iter().any(|diagnostic| {
            !matches!(diagnostic.severity.as_str(), "error" | "warning")
                || diagnostic.text.len() > 16 * 1024
                || diagnostic
                    .file
                    .as_ref()
                    .is_some_and(|path| path.len() > 4096)
        })
    {
        return Err(DenoGenUiError::Protocol(
            "compiler diagnostics exceed their schema bounds".into(),
        ));
    }
    Ok(())
}

fn verify_asset(path: &Path, expected: &str, label: &'static str) -> Result<(), DenoGenUiError> {
    let actual = sha256_file(path)?;
    if actual != expected {
        return Err(DenoGenUiError::AssetDigestMismatch {
            label,
            expected: expected.into(),
            actual,
        });
    }
    Ok(())
}

fn sha256_text_pair(left: &str, right: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(left.as_bytes());
    digest.update(right.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_virtual_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\\') && !path.contains("..")
}

fn path_text(path: &Path) -> Result<&str, DenoGenUiError> {
    path.to_str()
        .ok_or_else(|| DenoGenUiError::InvalidConfig("GenUI asset path is not UTF-8".into()))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, DenoGenUiError> {
    mutex.lock().map_err(|_| DenoGenUiError::LockPoisoned)
}

#[derive(Debug, Error)]
pub enum DenoGenUiError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("Deno GenUI filesystem setup failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid Deno GenUI configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid GenUI compile request: {0}")]
    InvalidRequest(String),
    #[error("Deno GenUI compiler is not ready")]
    NotReady,
    #[error("Deno GenUI request {request_id} timed out")]
    Timeout { request_id: String },
    #[error("Deno GenUI protocol failed: {0}")]
    Protocol(String),
    #[error("Deno GenUI compiler exited in state {state:?}: {stderr}")]
    Exited { state: DriverState, stderr: String },
    #[error("Deno GenUI compiler rejected the source: {0:?}")]
    CompileFailed(Vec<GenUiCompileDiagnostic>),
    #[error("{label} digest mismatch: expected {expected}, got {actual}")]
    AssetDigestMismatch {
        label: &'static str,
        expected: String,
        actual: String,
    },
    #[error("artifact digest mismatch: expected {expected}, got {actual}")]
    ArtifactDigestMismatch { expected: String, actual: String },
    #[error("Deno GenUI lock was poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> GenUiCompileRequest {
        GenUiCompileRequest {
            source_revision: 7,
            entrypoint: "/App.tsx".into(),
            files: BTreeMap::from([(
                "/App.tsx".into(),
                "export default function App() { return <main />; }".into(),
            )]),
        }
    }

    #[test]
    fn request_paths_and_source_size_are_bounded_before_spawn() {
        let mut invalid = request();
        invalid.files.insert("../secret.ts".into(), "secret".into());
        assert!(matches!(
            validate_request(&invalid),
            Err(DenoGenUiError::InvalidRequest(message)) if message.contains("virtual source path")
        ));

        let mut oversized = request();
        oversized
            .files
            .insert("/large.ts".into(), "x".repeat(MAX_GENUI_SOURCE_BYTES + 1));
        assert!(matches!(
            validate_request(&oversized),
            Err(DenoGenUiError::InvalidRequest(message)) if message.contains("exceeds")
        ));
    }

    #[test]
    fn artifact_digest_is_recomputed_by_rust() {
        let request = request();
        let mut candidate = GenUiArtifactCandidate {
            schema_version: 1,
            source_revision: request.source_revision,
            entrypoint: request.entrypoint.clone(),
            bundle: "bundle".into(),
            css: "css".into(),
            source_map: "{}".into(),
            content_digest: sha256_text_pair("bundle", "css"),
            compiler: GenUiCompilerIdentity {
                name: "esbuild-wasm".into(),
                version: "0.28.1".into(),
            },
            diagnostics: vec![],
        };
        validate_candidate(&candidate, &request, "0.28.1").unwrap();
        candidate.content_digest = "0".repeat(64);
        assert!(matches!(
            validate_candidate(&candidate, &request, "0.28.1"),
            Err(DenoGenUiError::ArtifactDigestMismatch { .. })
        ));
    }
}
