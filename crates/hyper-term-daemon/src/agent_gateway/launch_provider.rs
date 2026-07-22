use super::*;

impl AgentGatewayRuntime {
    pub(super) fn launch_provider(
        &self,
        provider_id: &str,
        session_root: &std::path::Path,
        mut mcp: Option<BrokeredMcpLaunch>,
    ) -> Result<LaunchedAgentProvider, StartError> {
        if provider_id == "codex" {
            let executable = self
                .config
                .codex_executable
                .as_ref()
                .ok_or(StartError::Unavailable)?
                .canonicalize()
                .map_err(|_| StartError::Unavailable)?;
            let executable_sha256 = sha256_file(&executable).map_err(|_| StartError::Driver)?;
            let managed_proxy = ManagedConnectProxy::start(
                CODEX_NETWORK_ALLOWED_HOSTS
                    .iter()
                    .map(|host| (*host).to_owned()),
            )
            .map_err(|_| StartError::Driver)?;
            let endpoint = managed_proxy.endpoint();
            let allowed_unix_sockets = mcp
                .as_ref()
                .map(|mcp| vec![mcp.capability_socket.clone()])
                .unwrap_or_default();
            let read_paths = Vec::new();
            let auth_file = self
                .config
                .codex_auth_file
                .as_deref()
                .filter(|path| path.is_file())
                .map(Path::to_owned);
            let capability_server = mcp.as_mut().and_then(|mcp| mcp.capability_server.take());
            let client = CodexAppServerClient::launch(CodexAppServerConfig {
                executable,
                executable_sha256,
                implementation_version: "installed".into(),
                workspace: self.config.workspace.clone(),
                codex_home: session_root.join("codex-home"),
                scratch_directory: session_root.join("scratch"),
                auth_file,
                brokered_mcp_server: mcp.map(|mcp| CodexMcpServerConfig {
                    executable: mcp.executable,
                    executable_sha256: mcp.executable_sha256,
                    arguments: mcp.arguments,
                }),
                containment: Some(AgentContainmentConfig {
                    proxy_url: endpoint.proxy_url.clone(),
                    credentialed_proxy_url: managed_proxy.credentialed_proxy_url().to_owned(),
                    allowed_hosts: endpoint.allowed_hosts.clone(),
                    allowed_unix_sockets,
                    read_paths,
                    write_paths: vec![session_root.to_path_buf()],
                }),
            })
            .map_err(|error| {
                if agent_diagnostics_enabled() {
                    eprintln!(
                        "hyper-term-agent: {provider_id} launch failed: {}",
                        bounded_agent_diagnostic(&error.to_string()),
                    );
                }
                StartError::Driver
            })?;
            return Ok(LaunchedAgentProvider {
                client: Arc::new(client),
                managed_proxy: Some(managed_proxy),
                capability_server,
            });
        }
        let provider = self
            .config
            .acp_providers
            .iter()
            .find(|provider| provider.provider_id == provider_id)
            .ok_or(StartError::Unavailable)?;
        let executable_sha256 =
            sha256_file(&provider.executable).map_err(|_| StartError::Driver)?;
        let allowed_hosts =
            acp_network_allowed_hosts(&provider.provider_id).ok_or(StartError::Unavailable)?;
        let managed_proxy =
            ManagedConnectProxy::start(allowed_hosts.iter().map(|host| (*host).to_owned()))
                .map_err(|_| StartError::Driver)?;
        let endpoint = managed_proxy.endpoint();
        let allowed_unix_sockets = mcp
            .as_ref()
            .map(|mcp| vec![mcp.capability_socket.clone()])
            .unwrap_or_default();
        let mut read_paths = acp_provider_read_paths(provider);
        let mut environment = provider.environment.clone();
        if provider.provider_id == "codex-acp" {
            let isolated_home = session_root.join("home");
            let codex_home = session_root.join("codex-home");
            let scratch = session_root.join("scratch");
            for directory in [&isolated_home, &codex_home, &scratch] {
                create_private_runtime_root(directory).map_err(|_| StartError::Driver)?;
            }
            let auth_file = self
                .config
                .codex_auth_file
                .as_deref()
                .filter(|path| path.is_file());
            stage_codex_auth_file(auth_file, &codex_home).map_err(|_| StartError::Driver)?;
            if let Some(auth_file) = auth_file {
                let mut seen = read_paths.iter().cloned().collect::<HashSet<_>>();
                add_existing_acp_read_path(&mut read_paths, &mut seen, auth_file);
            }
            if let Some(home) = provider.environment.get("HOME") {
                stage_acp_codex_preferences(Path::new(home), &codex_home)
                    .map_err(|_| StartError::Driver)?;
            }
            environment.insert("HOME".into(), isolated_home.into_os_string());
            environment.insert("CODEX_HOME".into(), codex_home.into_os_string());
            environment.insert("TMPDIR".into(), scratch.into_os_string());
        } else if provider.provider_id == "claude-acp" {
            let isolated_home = session_root.join("home");
            let scratch = session_root.join("scratch");
            for directory in [&isolated_home, &scratch] {
                create_private_runtime_root(directory).map_err(|_| StartError::Driver)?;
            }
            if let Some(home) = provider.environment.get("HOME") {
                stage_acp_claude_home(Path::new(home), &isolated_home)
                    .map_err(|_| StartError::Driver)?;
            }
            environment.insert("HOME".into(), isolated_home.into_os_string());
            environment.remove("CLAUDE_CONFIG_DIR");
            environment.insert("TMPDIR".into(), scratch.clone().into_os_string());
            environment.insert("CLAUDE_CODE_TMPDIR".into(), scratch.into_os_string());
            if agent_diagnostics_enabled() {
                environment.insert("DEBUG_CLAUDE_AGENT_SDK".into(), "1".into());
            }
        } else if provider.provider_id == "copilot-acp" {
            let isolated_home = session_root.join("home");
            let scratch = session_root.join("scratch");
            for directory in [&isolated_home, &scratch] {
                create_private_runtime_root(directory).map_err(|_| StartError::Driver)?;
            }
            if let Some(home) = provider.environment.get("HOME") {
                stage_acp_copilot_home(Path::new(home), &isolated_home)
                    .map_err(|_| StartError::Driver)?;
            }
            environment.insert("HOME".into(), isolated_home.clone().into_os_string());
            environment.insert(
                "COPILOT_HOME".into(),
                isolated_home.join(".copilot").into_os_string(),
            );
            environment.insert("TMPDIR".into(), scratch.into_os_string());
        }
        let capability_server = mcp.as_mut().and_then(|mcp| mcp.capability_server.take());
        let client = AcpAgentClient::launch(AcpAgentConfig {
            executable: provider.executable.clone(),
            executable_sha256,
            arguments: provider.arguments.clone(),
            environment,
            implementation_version: provider.implementation_version.clone(),
            provider_id: provider.provider_id.clone(),
            workspace: self.config.workspace.clone(),
            brokered_mcp_server: mcp.map(|mcp| AcpMcpServerConfig {
                executable: mcp.executable,
                executable_sha256: mcp.executable_sha256,
                arguments: mcp.arguments,
                runtime_home: mcp.runtime_home,
                runtime_temp: mcp.runtime_temp,
            }),
            containment: Some(AgentContainmentConfig {
                proxy_url: endpoint.proxy_url.clone(),
                credentialed_proxy_url: managed_proxy.credentialed_proxy_url().to_owned(),
                allowed_hosts: endpoint.allowed_hosts.clone(),
                allowed_unix_sockets,
                read_paths,
                write_paths: vec![session_root.to_path_buf()],
            }),
            terminal_client: self.config.tier2_runner.is_some(),
        })
        .map_err(|error| {
            if agent_diagnostics_enabled() {
                eprintln!(
                    "hyper-term-agent: {provider_id} launch failed: {}",
                    bounded_agent_diagnostic(&error.to_string()),
                );
            }
            StartError::Driver
        })?;
        Ok(LaunchedAgentProvider {
            client: Arc::new(client),
            managed_proxy: Some(managed_proxy),
            capability_server,
        })
    }
}
