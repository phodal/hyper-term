use super::*;

impl AgentGatewayRuntime {
    pub(super) fn mcp_launch(
        &self,
        task_id: TaskId,
        session_root: &std::path::Path,
    ) -> Option<Result<BrokeredMcpLaunch, StartError>> {
        let executable = match self.config.mcp_executable.as_ref()?.canonicalize() {
            Ok(executable) => executable,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let digest = match sha256_file(&executable) {
            Ok(digest) => digest,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let runtime_home = session_root.join("mcp-home");
        let runtime_temp = session_root.join("mcp-tmp");
        for directory in [&runtime_home, &runtime_temp] {
            if create_private_runtime_root(directory).is_err() {
                return Some(Err(StartError::Driver));
            }
        }
        // AF_UNIX paths are short on macOS. Keep the endpoint beside the
        // desktop control socket, outside the provider-writable session tree;
        // the full task identity remains bound in the Rust server state.
        let task_key = task_id.to_string();
        let capability_socket = self
            .config
            .control_socket
            .parent()?
            .join(".acp")
            .join(&task_key[..16]);
        let capability_server = match spawn_agent_capability_server(
            &capability_socket,
            self.config.daemon.clone(),
            task_id,
        ) {
            Ok(server) => server,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let capability_socket = match capability_socket.canonicalize() {
            Ok(socket) => socket,
            Err(_) => return Some(Err(StartError::Driver)),
        };
        let mut arguments = vec![
            "--agent-mode".into(),
            "--socket".into(),
            capability_socket.clone().into_os_string(),
            "--task-id".into(),
            task_id.to_string().into(),
        ];
        let mut registration = BrokeredMcpRuntimeConfig::default();
        if let Some(runtime) = &self.config.genui_runtime {
            let deno_sha256 = match sha256_file(&runtime.deno_executable) {
                Ok(digest) => digest,
                Err(_) => return Some(Err(StartError::Driver)),
            };
            let script_sha256 = match sha256_file(&runtime.compiler_script) {
                Ok(digest) => digest,
                Err(_) => return Some(Err(StartError::Driver)),
            };
            let wasm_sha256 = match sha256_file(&runtime.compiler_wasm) {
                Ok(digest) => digest,
                Err(_) => return Some(Err(StartError::Driver)),
            };
            let deno_root = self.brokered_mcp_root(task_id);
            if create_private_runtime_root(&deno_root).is_err() {
                return Some(Err(StartError::Driver));
            }
            let snapshot = create_workspace_snapshot(
                &self.config.workspace,
                &deno_root.join("workspace-snapshot"),
            )
            .ok();
            let lsp_cache = deno_root.join("lsp-cache");
            let lsp_scratch = deno_root.join("lsp-scratch");
            let genui_cache = deno_root.join("genui-cache");
            let genui_scratch = deno_root.join("genui-scratch");
            for directory in [&lsp_cache, &lsp_scratch, &genui_cache, &genui_scratch] {
                if create_private_runtime_root(directory).is_err() {
                    return Some(Err(StartError::Driver));
                }
            }
            registration = BrokeredMcpRuntimeConfig {
                deno_lsp: snapshot.map(|snapshot| DenoMcpExecutorConfig {
                    executable: runtime.deno_executable.clone(),
                    executable_sha256: deno_sha256.clone(),
                    runtime_version: runtime.runtime_version.clone(),
                    workspace_snapshot: snapshot.root,
                    cache_directory: lsp_cache,
                    scratch_directory: lsp_scratch,
                }),
                deno_genui: Some(DenoGenUiMcpExecutorConfig {
                    executable: runtime.deno_executable.clone(),
                    executable_sha256: deno_sha256,
                    runtime_version: runtime.runtime_version.clone(),
                    compiler_script: runtime.compiler_script.clone(),
                    compiler_script_sha256: script_sha256,
                    compiler_wasm: runtime.compiler_wasm.clone(),
                    compiler_wasm_sha256: wasm_sha256,
                    compiler_version: runtime.compiler_version.clone(),
                    cache_directory: genui_cache,
                    scratch_directory: genui_scratch,
                }),
            };
            let lsp_enabled = registration.deno_lsp.is_some();
            if lsp_enabled {
                arguments.push("--enable-deno-lsp".into());
            }
            arguments.push("--enable-genui".into());
        }
        if self
            .config
            .daemon
            .register_brokered_mcp_runtime(task_id, registration)
            .is_err()
        {
            let _ = std::fs::remove_dir_all(self.brokered_mcp_root(task_id));
            return Some(Err(StartError::Driver));
        }
        Some(Ok(BrokeredMcpLaunch {
            executable,
            executable_sha256: digest,
            arguments,
            runtime_home,
            runtime_temp,
            capability_socket,
            capability_server: Some(capability_server),
        }))
    }
}
