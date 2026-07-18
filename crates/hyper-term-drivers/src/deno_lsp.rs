use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    DEFAULT_MAX_DRIVER_FRAME_BYTES, DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES, DriverError,
    DriverEvent, DriverFraming, DriverKind, DriverManifest, DriverProcess, DriverSpec, DriverState,
    deno_containment::compile_deno_task_sandbox,
    process::{BoundedDriverInbox, sandbox_permission_profile},
};

const MAX_BUFFERED_LSP_EVENTS: usize = 512;
const MAX_BUFFERED_LSP_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

pub struct DenoLspConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub runtime_version: String,
    pub workspace_snapshot: PathBuf,
    pub cache_directory: PathBuf,
    pub scratch_directory: PathBuf,
}

pub struct DenoLspClient {
    process: DriverProcess,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    inbox: Mutex<BoundedDriverInbox>,
    workspace_uri: String,
}

impl DenoLspClient {
    pub fn launch(config: DenoLspConfig) -> Result<Self, DenoLspError> {
        if !config.executable.is_absolute()
            || !config.workspace_snapshot.is_absolute()
            || !config.cache_directory.is_absolute()
            || !config.scratch_directory.is_absolute()
        {
            return Err(DenoLspError::InvalidConfig(
                "Deno LSP directories must be absolute".into(),
            ));
        }
        fs::create_dir_all(&config.cache_directory)?;
        fs::create_dir_all(&config.scratch_directory)?;
        let workspace = config.workspace_snapshot.canonicalize()?;
        let cache = config.cache_directory.canonicalize()?;
        let scratch = config.scratch_directory.canonicalize()?;
        let workspace_uri = path_to_file_uri(&workspace)?;
        let environment = BTreeMap::from([
            ("DENO_DIR".into(), cache.clone().into_os_string()),
            ("DENO_NO_PROMPT".into(), OsString::from("1")),
            ("DENO_NO_UPDATE_CHECK".into(), OsString::from("1")),
            ("HOME".into(), scratch.clone().into_os_string()),
            ("NO_COLOR".into(), OsString::from("1")),
            ("TMPDIR".into(), scratch.clone().into_os_string()),
        ]);
        let arguments = vec![OsString::from("lsp"), OsString::from("--quiet")];
        let driver_id = Uuid::new_v4();
        let sandbox = compile_deno_task_sandbox(
            driver_id,
            &config.executable,
            &arguments,
            &workspace,
            &environment,
            [workspace.clone()],
            [cache, scratch],
        )?;
        let permission_profile = sandbox_permission_profile(&sandbox);
        let process = DriverProcess::spawn(DriverSpec {
            manifest: DriverManifest {
                driver_id,
                kind: DriverKind::DenoLsp,
                implementation_version: config.runtime_version,
                protocol_version: "lsp-3.17+deno".into(),
                capabilities: vec![
                    "diagnostics".into(),
                    "completion".into(),
                    "formatting".into(),
                    "virtual_text_document".into(),
                ],
                transport: "stdio-content-length".into(),
                executable_sha256: config.executable_sha256,
                permission_profile,
            },
            executable: config.executable,
            arguments,
            working_directory: workspace,
            environment,
            sandbox: Some(sandbox),
            framing: DriverFraming::ContentLength,
            max_frame_bytes: DEFAULT_MAX_DRIVER_FRAME_BYTES,
            max_pending_output_bytes: DEFAULT_MAX_PENDING_DRIVER_OUTPUT_BYTES,
        })?;
        Ok(Self {
            process,
            next_request_id: AtomicU64::new(1),
            request_gate: Mutex::new(()),
            inbox: Mutex::new(BoundedDriverInbox::new(
                MAX_BUFFERED_LSP_EVENTS,
                MAX_BUFFERED_LSP_OUTPUT_BYTES,
            )),
            workspace_uri,
        })
    }

    pub fn initialize(&self, timeout: Duration) -> Result<Value, DenoLspError> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        if let Err(error) = self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": {"name": "hyper-term", "version": env!("CARGO_PKG_VERSION")},
                "locale": "en",
                "rootUri": self.workspace_uri,
                "workspaceFolders": [{"uri": self.workspace_uri, "name": "workspace-snapshot"}],
                "capabilities": {},
                "initializationOptions": {
                    "enable": true,
                    "lint": true,
                    "unstable": false
                }
            }
        })) {
            let _ = self.process.stop(Duration::from_millis(100));
            return Err(error.into());
        }
        let response = match self.wait_for_response(id, timeout) {
            Ok(response) => response,
            Err(error) => {
                let _ = self.process.stop(Duration::from_millis(100));
                return Err(error);
            }
        };
        self.process.mark_ready()?;
        self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }))?;
        Ok(response)
    }

    pub fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, DenoLspError> {
        self.request_inner(method, Some(params), timeout)
    }

    fn request_inner(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, DenoLspError> {
        if method.is_empty() || method.len() > 256 {
            return Err(DenoLspError::InvalidConfig(
                "LSP request method is empty or oversized".into(),
            ));
        }
        let _gate = lock(&self.request_gate)?;
        if self.process.state()? != DriverState::Ready {
            return Err(DenoLspError::NotReady);
        }
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let mut request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method
        });
        if let Some(params) = params {
            request["params"] = params;
        }
        self.process.begin_effect()?;
        if let Err(error) = self.process.send_json(&request) {
            let _ = self.process.stop(Duration::from_millis(100));
            return Err(error.into());
        }
        match self.wait_for_response(id, timeout) {
            Ok(response) => {
                self.process.finish_effect()?;
                Ok(response)
            }
            Err(error @ DenoLspError::Remote { .. }) => {
                self.process.finish_effect()?;
                Err(error)
            }
            Err(error) => {
                let _ = self.process.stop(Duration::from_millis(100));
                Err(error)
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), DenoLspError> {
        if self.process.state()? != DriverState::Ready {
            return Err(DenoLspError::NotReady);
        }
        self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))?;
        Ok(())
    }

    pub fn respond(&self, id: Value, result: Value) -> Result<(), DenoLspError> {
        if self.process.state()? != DriverState::Ready {
            return Err(DenoLspError::NotReady);
        }
        if !(id.is_string() || id.is_i64() || id.is_u64()) {
            return Err(DenoLspError::InvalidConfig(
                "LSP response ID must be a string or integer".into(),
            ));
        }
        self.process.send_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }))?;
        Ok(())
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<DriverEvent, DenoLspError> {
        if let Some(event) = lock(&self.inbox)?.pop_front() {
            return Ok(event);
        }
        Ok(self.process.recv_timeout(timeout)?)
    }

    pub fn state(&self) -> Result<DriverState, DenoLspError> {
        Ok(self.process.state()?)
    }

    pub fn stderr_tail(&self) -> Result<String, DenoLspError> {
        Ok(self.process.stderr_tail()?)
    }

    pub fn shutdown(&self, timeout: Duration) -> Result<DriverState, DenoLspError> {
        if self.process.state()? == DriverState::Ready {
            let _ = self.request_inner("shutdown", None, timeout)?;
            self.process.send_json(&json!({
                "jsonrpc": "2.0",
                "method": "exit",
                "params": null
            }))?;
        }
        Ok(self.process.stop(Duration::from_millis(250))?)
    }

    fn wait_for_response(&self, id: u64, timeout: Duration) -> Result<Value, DenoLspError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(DenoLspError::Timeout { request_id: id });
            }
            let event = match self.process.recv_timeout(remaining) {
                Ok(event) => event,
                Err(DriverError::Timeout | DriverError::EffectTimedOut { .. }) => {
                    return Err(DenoLspError::Timeout { request_id: id });
                }
                Err(error) => return Err(error.into()),
            };
            match event {
                DriverEvent::Message { ref payload, .. }
                    if server_request_response(payload, &self.workspace_uri).is_some() =>
                {
                    let response = server_request_response(payload, &self.workspace_uri)
                        .expect("guard checked the response");
                    self.process.send_json(&response)?;
                }
                DriverEvent::Message { ref payload, .. }
                    if payload.get("id") == Some(&Value::from(id)) =>
                {
                    if let Some(error) = payload.get("error") {
                        return Err(DenoLspError::Remote {
                            request_id: id,
                            error: error.clone(),
                        });
                    }
                    return Ok(event_payload(event));
                }
                DriverEvent::ProtocolError { ref message } => {
                    return Err(DenoLspError::Protocol(message.clone()));
                }
                DriverEvent::Exited { state, .. } => {
                    return Err(DenoLspError::Exited(state));
                }
                event => lock(&self.inbox)?
                    .push_back(event)
                    .map_err(|_| DenoLspError::InboxOverflow)?,
            }
        }
    }
}

fn server_request_response(payload: &Value, workspace_uri: &str) -> Option<Value> {
    let id = payload.get("id")?;
    if !(id.is_string() || id.is_i64() || id.is_u64()) {
        return None;
    }
    let result = match payload.get("method").and_then(Value::as_str)? {
        "workspace/configuration" => {
            let count = payload
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(1, Vec::len);
            Value::Array(
                (0..count)
                    .map(|_| json!({"enable": true, "lint": true, "unstable": false}))
                    .collect(),
            )
        }
        "workspace/workspaceFolders" => {
            json!([{"uri": workspace_uri, "name": "workspace-snapshot"}])
        }
        "window/workDoneProgress/create" | "client/registerCapability" => Value::Null,
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": id.clone(), "result": result}))
}

fn event_payload(event: DriverEvent) -> Value {
    match event {
        DriverEvent::Message { payload, .. } => payload,
        _ => unreachable!("caller selects message events"),
    }
}

pub fn path_to_file_uri(path: &Path) -> Result<String, DenoLspError> {
    let value = path
        .to_str()
        .ok_or_else(|| DenoLspError::InvalidConfig("workspace path is not UTF-8".into()))?
        .replace('\\', "/");
    let mut result = String::from("file://");
    if !value.starts_with('/') {
        result.push('/');
    }
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':') {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(result)
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, DenoLspError> {
    mutex.lock().map_err(|_| DenoLspError::LockPoisoned)
}

#[derive(Debug, Error)]
pub enum DenoLspError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("Deno LSP filesystem setup failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid Deno LSP configuration: {0}")]
    InvalidConfig(String),
    #[error("Deno LSP has not completed initialization")]
    NotReady,
    #[error("Deno LSP request {request_id} timed out")]
    Timeout { request_id: u64 },
    #[error("Deno LSP request {request_id} failed: {error}")]
    Remote { request_id: u64, error: Value },
    #[error("Deno LSP protocol failed: {0}")]
    Protocol(String),
    #[error("Deno LSP exited in state {0:?}")]
    Exited(DriverState),
    #[error("Deno LSP event inbox exceeded its bound")]
    InboxOverflow,
    #[error("Deno LSP lock was poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_encodes_workspace_paths() {
        let uri = path_to_file_uri(Path::new("/tmp/hyper term/界面")).unwrap();
        assert_eq!(uri, "file:///tmp/hyper%20term/%E7%95%8C%E9%9D%A2");
    }

    #[test]
    fn fixed_lsp_configuration_requests_are_answered_without_renderer_input() {
        let response = server_request_response(
            &json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "workspace/configuration",
                "params": {"items": [{"section": "deno"}, {"section": "typescript"}]}
            }),
            "file:///snapshot",
        )
        .unwrap();
        assert_eq!(response["id"], 4);
        assert_eq!(response["result"].as_array().unwrap().len(), 2);
        assert_eq!(response["result"][0]["enable"], true);
    }
}
