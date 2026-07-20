use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hyper_term_core::OperationRecord;
use hyper_term_drivers::{
    LocalMcpClientError, LocalMcpPlanError, LocalMcpServerConfig, LocalMcpToolCallError,
    LocalMcpToolExecution, ManagedLocalMcpClient, PreparedLocalMcpServer, PreparedLocalMcpToolCall,
    authorize_local_mcp_server, authorize_local_mcp_tool_call, prepare_local_mcp_server,
    prepare_local_mcp_tool_call,
};
use hyper_term_protocol::{
    LocalMcpCredentialScope, LocalMcpServerLifecycle, LocalMcpServerRuntimeReceipt,
    OperationAction, OperationCompletion, OperationId, OperationKind, OperationOutcome,
    PermissionDecision, RiskClass, TaskId,
};
use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use crate::{DaemonError, DaemonState};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RegisteredLocalMcpServer {
    pub server_id: String,
    pub runtime_identity_digest: String,
    pub lifecycle: LocalMcpServerLifecycle,
    pub credential_scope: LocalMcpCredentialScope,
}

#[derive(Clone)]
pub struct LocalMcpRuntimeManager {
    daemon: DaemonState,
    state: Arc<Mutex<ManagerState>>,
}

struct ManagerState {
    registered: BTreeMap<String, PreparedLocalMcpServer>,
    pending_launches: HashMap<OperationId, PendingLaunch>,
    pending_calls: HashMap<OperationId, PendingCall>,
    clients: HashMap<RuntimeKey, Arc<AsyncMutex<ManagedLocalMcpClient>>>,
}

#[derive(Clone)]
struct PendingLaunch {
    task_id: TaskId,
    server_id: String,
    prepared: PreparedLocalMcpServer,
}

#[derive(Clone)]
struct PendingCall {
    task_id: TaskId,
    key: RuntimeKey,
    prepared: PreparedLocalMcpToolCall,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RuntimeKey {
    task_id: TaskId,
    server_id: String,
}

impl LocalMcpRuntimeManager {
    pub fn new(
        daemon: DaemonState,
        configs: impl IntoIterator<Item = LocalMcpServerConfig>,
    ) -> Result<Self, LocalMcpRuntimeError> {
        let mut registered = BTreeMap::new();
        for config in configs {
            let prepared = prepare_local_mcp_server(config)?;
            let server_id = prepared.launch.server_id.clone();
            if registered.insert(server_id.clone(), prepared).is_some() {
                return Err(LocalMcpRuntimeError::DuplicateServer(server_id));
            }
        }
        Ok(Self {
            daemon,
            state: Arc::new(Mutex::new(ManagerState {
                registered,
                pending_launches: HashMap::new(),
                pending_calls: HashMap::new(),
                clients: HashMap::new(),
            })),
        })
    }

    pub fn registered_servers(
        &self,
    ) -> Result<Vec<RegisteredLocalMcpServer>, LocalMcpRuntimeError> {
        let state = self.lock()?;
        Ok(state
            .registered
            .values()
            .map(|prepared| RegisteredLocalMcpServer {
                server_id: prepared.launch.server_id.clone(),
                runtime_identity_digest: prepared.launch.runtime_identity_digest.to_string(),
                lifecycle: prepared.launch.lifecycle,
                credential_scope: prepared.launch.credential_scope,
            })
            .collect())
    }

    pub fn propose_launch(
        &self,
        task_id: TaskId,
        server_id: &str,
    ) -> Result<OperationRecord, LocalMcpRuntimeError> {
        let mut state = self.lock()?;
        let key = RuntimeKey {
            task_id,
            server_id: server_id.to_owned(),
        };
        if state.clients.contains_key(&key)
            || state.pending_launches.values().any(|pending| {
                pending.task_id == task_id && pending.server_id.as_str() == server_id
            })
        {
            return Err(LocalMcpRuntimeError::ServerAlreadyActive);
        }
        let prepared = state
            .registered
            .get(server_id)
            .cloned()
            .ok_or(LocalMcpRuntimeError::UnknownServer)?;
        let operation = self.daemon.propose_operation(
            task_id,
            OperationKind::McpServerLaunch,
            OperationAction::McpServerLaunch {
                launch: prepared.launch.clone(),
            },
            format!("Start reviewed local MCP server {server_id}"),
            RiskClass::ExternalEffect,
            vec!["mcp.server.launch".into()],
        )?;
        state.pending_launches.insert(
            operation.operation_id,
            PendingLaunch {
                task_id,
                server_id: server_id.to_owned(),
                prepared,
            },
        );
        Ok(operation)
    }

    pub fn has_pending_launch(
        &self,
        operation_id: OperationId,
    ) -> Result<bool, LocalMcpRuntimeError> {
        Ok(self.lock()?.pending_launches.contains_key(&operation_id))
    }

    pub async fn resolve_launch(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        decision: PermissionDecision,
    ) -> Result<Option<LocalMcpServerRuntimeReceipt>, LocalMcpRuntimeError> {
        validate_decision(decision)?;
        let pending = self
            .lock()?
            .pending_launches
            .get(&operation_id)
            .filter(|pending| pending.task_id == task_id)
            .cloned()
            .ok_or(LocalMcpRuntimeError::PendingLaunchMissing)?;
        let decided =
            self.daemon
                .decide_permission(task_id, operation_id, expected_revision, decision)?;
        if decision != PermissionDecision::AllowOnce {
            self.lock()?.pending_launches.remove(&operation_id);
            return Ok(None);
        }
        let dispatching = match self
            .daemon
            .begin_operation(task_id, operation_id, decided.revision)
        {
            Ok(dispatching) => dispatching,
            Err(error) => {
                self.lock()?.pending_launches.remove(&operation_id);
                return Err(error.into());
            }
        };
        let result: Result<LocalMcpServerRuntimeReceipt, LocalMcpRuntimeError> = async {
            let authorized = authorize_local_mcp_server(pending.prepared, &dispatching)?;
            let client =
                ManagedLocalMcpClient::launch(authorized, LAUNCH_TIMEOUT, DISCOVERY_TIMEOUT)
                    .await?;
            let receipt = client.receipt().clone();
            self.daemon.record_local_mcp_server_runtime(
                task_id,
                operation_id,
                dispatching.revision,
                receipt.clone(),
            )?;
            let key = RuntimeKey {
                task_id,
                server_id: pending.server_id,
            };
            let client = Arc::new(AsyncMutex::new(client));
            {
                let mut state = self.lock()?;
                match state.clients.entry(key.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(client);
                    }
                    Entry::Occupied(_) => {
                        return Err(LocalMcpRuntimeError::ServerAlreadyActive);
                    }
                }
            }
            if let Err(error) = self.daemon.complete_operation(
                task_id,
                operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "hyper-term-local-mcp".into(),
                    succeeded: true,
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: format!(
                        "local MCP server {} negotiated {} tool(s)",
                        receipt.launch.server_id,
                        receipt.tools.len()
                    ),
                    result_digest: Some(receipt.runtime_identity_digest.to_string()),
                },
            ) {
                let client = self.lock()?.clients.remove(&key);
                if let Some(client) = client {
                    let _ = client.lock().await.close(Duration::from_secs(2)).await;
                }
                return Err(error.into());
            }
            Ok(receipt)
        }
        .await;
        self.lock()?.pending_launches.remove(&operation_id);
        if let Err(error) = &result {
            let _ = self.daemon.complete_operation(
                task_id,
                operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "hyper-term-local-mcp".into(),
                    succeeded: false,
                    outcome: Some(OperationOutcome::Failed),
                    summary: bounded_summary(&error.to_string()),
                    result_digest: None,
                },
            );
        }
        result.map(Some)
    }

    pub fn propose_tool_call(
        &self,
        task_id: TaskId,
        server_id: &str,
        tool_name: String,
        arguments: Map<String, Value>,
    ) -> Result<OperationRecord, LocalMcpRuntimeError> {
        let mut state = self.lock()?;
        let key = RuntimeKey {
            task_id,
            server_id: server_id.to_owned(),
        };
        let client = state
            .clients
            .get(&key)
            .cloned()
            .ok_or(LocalMcpRuntimeError::ServerNotActive)?;
        let runtime = client
            .try_lock()
            .map_err(|_| LocalMcpRuntimeError::ServerBusy)?;
        let prepared = prepare_local_mcp_tool_call(runtime.receipt(), tool_name, arguments)?;
        drop(runtime);
        let operation = self.daemon.propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::McpToolCall {
                call: prepared.call.clone(),
            },
            format!(
                "Invoke reviewed local MCP tool {} on {server_id}",
                prepared.call.tool_name
            ),
            RiskClass::ExternalEffect,
            vec!["mcp.tool.call".into()],
        )?;
        state.pending_calls.insert(
            operation.operation_id,
            PendingCall {
                task_id,
                key,
                prepared,
            },
        );
        Ok(operation)
    }

    pub fn has_pending_call(
        &self,
        operation_id: OperationId,
    ) -> Result<bool, LocalMcpRuntimeError> {
        Ok(self.lock()?.pending_calls.contains_key(&operation_id))
    }

    pub async fn resolve_tool_call(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        decision: PermissionDecision,
    ) -> Result<Option<LocalMcpToolExecution>, LocalMcpRuntimeError> {
        validate_decision(decision)?;
        let pending = self
            .lock()?
            .pending_calls
            .get(&operation_id)
            .filter(|pending| pending.task_id == task_id)
            .cloned()
            .ok_or(LocalMcpRuntimeError::PendingCallMissing)?;
        let decided =
            self.daemon
                .decide_permission(task_id, operation_id, expected_revision, decision)?;
        if decision != PermissionDecision::AllowOnce {
            self.lock()?.pending_calls.remove(&operation_id);
            return Ok(None);
        }
        let dispatching = match self
            .daemon
            .begin_operation(task_id, operation_id, decided.revision)
        {
            Ok(dispatching) => dispatching,
            Err(error) => {
                self.lock()?.pending_calls.remove(&operation_id);
                return Err(error.into());
            }
        };
        let result: Result<LocalMcpToolExecution, LocalMcpRuntimeError> = async {
            let client = self
                .lock()?
                .clients
                .get(&pending.key)
                .cloned()
                .ok_or(LocalMcpRuntimeError::ServerNotActive)?;
            let authorized = authorize_local_mcp_tool_call(pending.prepared, &dispatching)?;
            let execution = client
                .lock()
                .await
                .call_tool(authorized, TOOL_CALL_TIMEOUT)
                .await?;
            self.daemon.record_local_mcp_tool_call(
                task_id,
                operation_id,
                dispatching.revision,
                execution.receipt.clone(),
            )?;
            let succeeded = execution.receipt.succeeded;
            self.daemon.complete_operation(
                task_id,
                operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "hyper-term-local-mcp".into(),
                    succeeded,
                    outcome: Some(if succeeded {
                        OperationOutcome::Succeeded
                    } else {
                        OperationOutcome::Failed
                    }),
                    summary: format!(
                        "local MCP tool {} {}",
                        execution.receipt.call.tool_name,
                        if succeeded {
                            "completed"
                        } else {
                            "returned an error"
                        }
                    ),
                    result_digest: Some(execution.receipt.result_digest.to_string()),
                },
            )?;
            Ok(execution)
        }
        .await;
        self.lock()?.pending_calls.remove(&operation_id);
        if let Err(error) = &result {
            let _ = self.daemon.complete_operation(
                task_id,
                operation_id,
                dispatching.revision,
                OperationCompletion {
                    executor: "hyper-term-local-mcp".into(),
                    succeeded: false,
                    outcome: Some(OperationOutcome::UnknownExecution),
                    summary: bounded_summary(&error.to_string()),
                    result_digest: None,
                },
            );
        }
        result.map(Some)
    }

    pub async fn active_server_receipts(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<LocalMcpServerRuntimeReceipt>, LocalMcpRuntimeError> {
        let clients = self
            .lock()?
            .clients
            .iter()
            .filter(|(key, _)| key.task_id == task_id)
            .map(|(_, client)| Arc::clone(client))
            .collect::<Vec<_>>();
        let mut receipts = Vec::with_capacity(clients.len());
        for client in clients {
            receipts.push(client.lock().await.receipt().clone());
        }
        receipts.sort_by(|left, right| left.launch.server_id.cmp(&right.launch.server_id));
        Ok(receipts)
    }

    pub async fn close_task(&self, task_id: TaskId) {
        let (pending, clients) = if let Ok(mut state) = self.state.lock() {
            let pending = state
                .pending_launches
                .iter()
                .filter(|(_, pending)| pending.task_id == task_id)
                .map(|(operation_id, _)| *operation_id)
                .chain(
                    state
                        .pending_calls
                        .iter()
                        .filter(|(_, pending)| pending.task_id == task_id)
                        .map(|(operation_id, _)| *operation_id),
                )
                .collect::<Vec<_>>();
            state
                .pending_launches
                .retain(|_, pending| pending.task_id != task_id);
            state
                .pending_calls
                .retain(|_, pending| pending.task_id != task_id);
            let keys = state
                .clients
                .keys()
                .filter(|key| key.task_id == task_id)
                .cloned()
                .collect::<Vec<_>>();
            let clients = keys
                .into_iter()
                .filter_map(|key| state.clients.remove(&key))
                .collect::<Vec<_>>();
            (pending, clients)
        } else {
            (Vec::new(), Vec::new())
        };
        self.cancel_pending(pending);
        close_clients(clients).await;
    }

    pub async fn close_all(&self) {
        let (pending, clients) = if let Ok(mut state) = self.state.lock() {
            let pending = state
                .pending_launches
                .keys()
                .chain(state.pending_calls.keys())
                .copied()
                .collect::<Vec<_>>();
            state.pending_launches.clear();
            state.pending_calls.clear();
            let clients = state.clients.drain().map(|(_, client)| client).collect();
            (pending, clients)
        } else {
            (Vec::new(), Vec::new())
        };
        self.cancel_pending(pending);
        close_clients(clients).await;
    }

    fn cancel_pending(&self, operation_ids: Vec<OperationId>) {
        for operation_id in operation_ids {
            let Ok(operation) = self.daemon.operation(operation_id) else {
                continue;
            };
            if operation.state == hyper_term_protocol::OperationState::WaitingHuman {
                let _ = self.daemon.decide_permission(
                    operation.task_id,
                    operation_id,
                    operation.revision,
                    PermissionDecision::Cancelled,
                );
            }
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, ManagerState>, LocalMcpRuntimeError> {
        self.state.lock().map_err(|_| LocalMcpRuntimeError::Lock)
    }
}

async fn close_clients(clients: Vec<Arc<AsyncMutex<ManagedLocalMcpClient>>>) {
    for client in clients {
        let _ = client.lock().await.close(Duration::from_secs(2)).await;
    }
}

fn validate_decision(decision: PermissionDecision) -> Result<(), LocalMcpRuntimeError> {
    if matches!(
        decision,
        PermissionDecision::AllowOnce
            | PermissionDecision::RejectOnce
            | PermissionDecision::Cancelled
    ) {
        Ok(())
    } else {
        Err(LocalMcpRuntimeError::UnsupportedDecision)
    }
}

fn bounded_summary(message: &str) -> String {
    message.chars().take(512).collect()
}

#[derive(Debug, Error)]
pub enum LocalMcpRuntimeError {
    #[error("local MCP server id is not registered")]
    UnknownServer,
    #[error("local MCP server id is registered more than once: {0}")]
    DuplicateServer(String),
    #[error("local MCP server is already active or waiting for approval")]
    ServerAlreadyActive,
    #[error("local MCP server is not active for this task")]
    ServerNotActive,
    #[error("local MCP server is busy")]
    ServerBusy,
    #[error("local MCP launch proposal is no longer pending")]
    PendingLaunchMissing,
    #[error("local MCP tool proposal is no longer pending")]
    PendingCallMissing,
    #[error("local MCP supports only allow-once, reject-once, or cancel decisions")]
    UnsupportedDecision,
    #[error("local MCP runtime manager lock is poisoned")]
    Lock,
    #[error("local MCP launch plan is invalid: {0}")]
    Plan(#[from] LocalMcpPlanError),
    #[error("local MCP client failed: {0}")]
    Client(#[from] LocalMcpClientError),
    #[error("local MCP tool call failed: {0}")]
    Tool(#[from] LocalMcpToolCallError),
    #[error("local MCP authority update failed: {0}")]
    Daemon(#[from] DaemonError),
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    use hyper_term_core::{ExecutionContextInputs, compile_execution_context};
    use hyper_term_drivers::sha256_file;
    use hyper_term_protocol::{
        BindingLifetime, BindingScope, CollisionPolicy, EnvironmentBindingOrigin,
        EnvironmentBindingSpec, EnvironmentClass, EnvironmentPlan, EnvironmentSource,
        ExecutionContextSpec, ExecutionMode, LocalMcpCredentialScope, LocalMcpServerLifecycle,
        OperationState, OverridePolicy, RuntimeEnvironmentSpec, SandboxEnforcement,
        SandboxEnvironmentPolicy, SandboxFileSystemPolicy, SandboxLifetime, SandboxNetworkPolicy,
        SandboxProcessPolicy, SandboxProfile, SandboxResourceLimits, WorkspaceContextSpec,
    };
    use tempfile::TempDir;

    use super::*;

    fn fixture_config(temp: &TempDir) -> LocalMcpServerConfig {
        let executable = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let workspace = temp.path().join("workspace");
        let home = temp.path().join("home");
        let scratch = temp.path().join("scratch");
        for directory in [&workspace, &home, &scratch] {
            fs::create_dir_all(directory).unwrap();
        }
        let workspace = workspace.canonicalize().unwrap();
        let mut profile = SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy::default(),
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy::default(),
            process: SandboxProcessPolicy::default(),
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneTask,
        };
        profile
            .process
            .allowed_executables
            .push(PathBuf::from("/bin/bash").canonicalize().unwrap());
        let spec = ExecutionContextSpec {
            schema_version: hyper_term_protocol::EXECUTION_CONTEXT_SCHEMA_VERSION,
            context_id: "mcp:managed-fixture:1".into(),
            context_revision: 1,
            mode: ExecutionMode::Hermetic,
            workspace: WorkspaceContextSpec {
                root: workspace.clone(),
                working_directory: workspace.clone(),
                runtime_home: home.canonicalize().unwrap(),
                runtime_temp: scratch.canonicalize().unwrap(),
            },
            runtime: RuntimeEnvironmentSpec {
                path: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
                locale: "C.UTF-8".into(),
                timezone: "UTC".into(),
                terminal: "dumb".into(),
            },
            shell: None,
            environment: EnvironmentPlan {
                bindings: vec![EnvironmentBindingSpec {
                    target_name: "MCP_FIXTURE_LABEL".into(),
                    source: EnvironmentSource::Literal {
                        value: "manager-private".into(),
                    },
                    class: EnvironmentClass::ToolConfiguration,
                    origin: EnvironmentBindingOrigin::Invocation,
                    scope: BindingScope::ProcessTree,
                    lifetime: BindingLifetime::Server,
                    override_policy: OverridePolicy::Deny,
                }],
                collision_policy: CollisionPolicy::Deny,
            },
            credentials: Vec::new(),
            sandbox: Some(profile),
        };
        let (execution_context, _) =
            compile_execution_context(&spec, &ExecutionContextInputs::default()).unwrap();
        let script = r#"
printf 'managed fixture ready\n' >&2
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"managed-fixture","version":"1.0.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"fixture.read","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"manager result"}],"structuredContent":{"text":"manager result"},"isError":false}}'
      ;;
  esac
done
"#;
        LocalMcpServerConfig {
            server_id: "managed_fixture".into(),
            executable_sha256: sha256_file(&executable).unwrap(),
            executable,
            arguments: [OsString::from("-c"), OsString::from(script)].into(),
            working_directory: workspace,
            execution_context,
            roots_snapshot_sha256: Some("a".repeat(64)),
            lifecycle: LocalMcpServerLifecycle::OneTask,
            credential_scope: LocalMcpCredentialScope::ServerLifetime,
        }
    }

    #[tokio::test]
    async fn manager_owns_reviewed_launch_call_receipts_and_task_cleanup() {
        let temp = TempDir::new().unwrap();
        let daemon = DaemonState::open(temp.path().join("state")).unwrap();
        let task_id = daemon.create_task("managed local MCP".into()).unwrap();
        let manager = LocalMcpRuntimeManager::new(daemon.clone(), [fixture_config(&temp)]).unwrap();
        let registered = manager.registered_servers().unwrap();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].server_id, "managed_fixture");

        let launch = manager.propose_launch(task_id, "managed_fixture").unwrap();
        assert_eq!(launch.state, OperationState::WaitingHuman);
        assert!(matches!(
            manager.propose_launch(task_id, "managed_fixture"),
            Err(LocalMcpRuntimeError::ServerAlreadyActive)
        ));
        let runtime = manager
            .resolve_launch(
                task_id,
                launch.operation_id,
                launch.revision,
                PermissionDecision::AllowOnce,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(runtime.server_name, "managed-fixture");
        assert_eq!(runtime.tools[0].name, "fixture.read");
        assert!(!runtime.per_call_isolation);
        assert_eq!(
            daemon.operation(launch.operation_id).unwrap().state,
            OperationState::Succeeded
        );
        assert_eq!(
            manager.active_server_receipts(task_id).await.unwrap(),
            vec![runtime.clone()]
        );

        let arguments = serde_json::json!({"path":"README.md"})
            .as_object()
            .unwrap()
            .clone();
        let call = manager
            .propose_tool_call(task_id, "managed_fixture", "fixture.read".into(), arguments)
            .unwrap();
        assert_eq!(call.state, OperationState::WaitingHuman);
        let execution = manager
            .resolve_tool_call(
                task_id,
                call.operation_id,
                call.revision,
                PermissionDecision::AllowOnce,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(execution.receipt.succeeded);
        assert!(execution.receipt.has_structured_content);
        assert!(
            serde_json::to_string(&execution.result)
                .unwrap()
                .contains("manager result")
        );
        assert_eq!(
            daemon.operation(call.operation_id).unwrap().state,
            OperationState::Succeeded
        );
        assert!(
            daemon
                .local_mcp_tool_call_event(task_id, call.operation_id)
                .unwrap()
                .is_some()
        );

        let rejected = manager
            .propose_tool_call(
                task_id,
                "managed_fixture",
                "fixture.read".into(),
                Map::new(),
            )
            .unwrap();
        assert!(
            manager
                .resolve_tool_call(
                    task_id,
                    rejected.operation_id,
                    rejected.revision,
                    PermissionDecision::RejectOnce,
                )
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            daemon.operation(rejected.operation_id).unwrap().state,
            OperationState::Cancelled
        );

        manager.close_task(task_id).await;
        assert!(
            manager
                .active_server_receipts(task_id)
                .await
                .unwrap()
                .is_empty()
        );
        let waiting = manager.propose_launch(task_id, "managed_fixture").unwrap();
        manager.close_task(task_id).await;
        assert_eq!(
            daemon.operation(waiting.operation_id).unwrap().state,
            OperationState::Cancelled
        );
    }
}
