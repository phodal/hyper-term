use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hyper_term_core::{
    ExecutionContextInputs, classify_environment_name, compile_execution_context,
};
use hyper_term_protocol::{
    BindingLifetime, BindingScope, CollisionPolicy, ContextReceipt, CredentialRequirement,
    EXECUTION_CONTEXT_SCHEMA_VERSION, EnvironmentBindingOrigin, EnvironmentBindingSpec,
    EnvironmentPlan, EnvironmentSource, ExecutionContextSpec, ExecutionMode, OverridePolicy,
    ResolvedExecutionContext, RuntimeEnvironmentSpec, SandboxProfile, SecretReference,
    WorkspaceContextSpec,
};
use uuid::Uuid;

use crate::AgentCredentialBinding;
use crate::DriverError;

pub(crate) fn compile_agent_execution_context(
    driver_id: Uuid,
    provider_id: &str,
    workspace: &Path,
    environment: &BTreeMap<String, OsString>,
    sandbox: SandboxProfile,
    managed_proxy_url: &str,
    credential_bindings: &[AgentCredentialBinding],
) -> Result<(ResolvedExecutionContext, ContextReceipt), DriverError> {
    let home =
        environment_path(environment, "HOME").unwrap_or_else(|| workspace.join(".agent-home"));
    let temp =
        environment_path(environment, "TMPDIR").unwrap_or_else(|| workspace.join(".agent-tmp"));
    let path = environment
        .get("PATH")
        .map(std::env::split_paths)
        .map(Iterator::collect::<Vec<_>>)
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    let mut bindings = Vec::new();
    for (name, value) in environment {
        if matches!(
            name.as_str(),
            "PATH" | "HOME" | "TMPDIR" | "LANG" | "TZ" | "TERM"
        ) {
            continue;
        }
        let value = value.to_str().ok_or_else(|| {
            DriverError::InvalidContainment(format!(
                "Agent environment variable {name} is not UTF-8"
            ))
        })?;
        bindings.push(EnvironmentBindingSpec {
            target_name: name.clone(),
            source: EnvironmentSource::Literal {
                value: value.into(),
            },
            class: classify_environment_name(name),
            origin: EnvironmentBindingOrigin::Invocation,
            scope: BindingScope::ProcessTree,
            lifetime: BindingLifetime::Task,
            override_policy: OverridePolicy::Deny,
        });
    }
    bindings.push(EnvironmentBindingSpec {
        target_name: "NODE_USE_ENV_PROXY".into(),
        source: EnvironmentSource::Literal { value: "1".into() },
        class: classify_environment_name("NODE_USE_ENV_PROXY"),
        origin: EnvironmentBindingOrigin::Authority,
        scope: BindingScope::ProcessTree,
        lifetime: BindingLifetime::Task,
        override_policy: OverridePolicy::Deny,
    });
    let spec = ExecutionContextSpec {
        schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
        context_id: format!("agent:{provider_id}:{driver_id}"),
        context_revision: 1,
        mode: ExecutionMode::Hermetic,
        workspace: WorkspaceContextSpec {
            root: workspace.to_path_buf(),
            working_directory: workspace.to_path_buf(),
            runtime_home: home,
            runtime_temp: temp,
        },
        runtime: RuntimeEnvironmentSpec {
            path,
            locale: environment_string(environment, "LANG").unwrap_or_else(|| "C.UTF-8".into()),
            timezone: environment_string(environment, "TZ").unwrap_or_else(|| "UTC".into()),
            terminal: environment_string(environment, "TERM").unwrap_or_else(|| "dumb".into()),
        },
        shell: None,
        environment: EnvironmentPlan {
            bindings,
            collision_policy: CollisionPolicy::Deny,
        },
        credentials: std::iter::once(CredentialRequirement {
            binding_id: "managed-connect-proxy".into(),
            reference: SecretReference {
                provider_id: "hyper-term-daemon".into(),
                secret_id: "managed-connect-proxy-session".into(),
                version: None,
            },
            target_name: "HTTPS_PROXY".into(),
            audience: managed_proxy_url.into(),
            scope: BindingScope::ProcessTree,
            lifetime: BindingLifetime::Task,
        })
        .chain(
            credential_bindings
                .iter()
                .map(|binding| CredentialRequirement {
                    binding_id: format!("provider-credential:{}", binding.target_name),
                    reference: SecretReference {
                        provider_id: binding.provider_id.clone(),
                        secret_id: binding.secret_id.clone(),
                        version: None,
                    },
                    target_name: binding.target_name.clone(),
                    audience: binding.audience.clone(),
                    scope: BindingScope::ProcessTree,
                    lifetime: BindingLifetime::Task,
                }),
        )
        .collect(),
        sandbox: Some(sandbox),
    };
    compile_execution_context(&spec, &ExecutionContextInputs::default())
        .map_err(|error| DriverError::InvalidContainment(error.to_string()))
}

pub(crate) fn compile_mcp_execution_context(
    driver_id: Uuid,
    workspace: &Path,
    runtime_home: PathBuf,
    runtime_temp: PathBuf,
    provider_context: &ResolvedExecutionContext,
    sandbox: SandboxProfile,
) -> Result<(ResolvedExecutionContext, ContextReceipt), DriverError> {
    let spec = ExecutionContextSpec {
        schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
        context_id: format!("mcp:hyper_term:{driver_id}"),
        context_revision: 1,
        mode: ExecutionMode::Hermetic,
        workspace: WorkspaceContextSpec {
            root: workspace.to_path_buf(),
            working_directory: workspace.to_path_buf(),
            runtime_home,
            runtime_temp,
        },
        runtime: provider_context.runtime.clone(),
        shell: None,
        environment: EnvironmentPlan::default(),
        credentials: Vec::new(),
        sandbox: Some(sandbox),
    };
    compile_execution_context(&spec, &ExecutionContextInputs::default())
        .map_err(|error| DriverError::InvalidContainment(error.to_string()))
}

pub(crate) fn os_environment(context: &ResolvedExecutionContext) -> BTreeMap<String, OsString> {
    context
        .environment
        .variables
        .iter()
        .map(|(name, value)| (name.clone(), OsString::from(value)))
        .collect()
}

fn environment_path(environment: &BTreeMap<String, OsString>, name: &str) -> Option<PathBuf> {
    environment.get(name).map(PathBuf::from)
}

fn environment_string(environment: &BTreeMap<String, OsString>, name: &str) -> Option<String> {
    environment
        .get(name)
        .and_then(|value| value.to_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::{
        SandboxEnforcement, SandboxEnvironmentPolicy, SandboxFileSystemPolicy, SandboxLifetime,
        SandboxNetworkPolicy, SandboxProcessPolicy, SandboxResourceLimits,
    };

    use super::*;

    #[test]
    fn provider_context_separates_proxy_reference_from_materialized_credentials() {
        let workspace = PathBuf::from("/tmp/hyper-provider-context");
        let profile = SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy::default(),
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy::default(),
            platform: Default::default(),
            process: SandboxProcessPolicy::default(),
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneTask,
        };
        let (resolved, receipt) = compile_agent_execution_context(
            Uuid::nil(),
            "codex-acp",
            &workspace,
            &BTreeMap::from([("ACP_MODE".into(), OsString::from("stdio"))]),
            profile,
            "http://127.0.0.1:43128",
            &[],
        )
        .unwrap();
        assert_eq!(
            resolved
                .environment
                .variables
                .get("NODE_USE_ENV_PROXY")
                .map(String::as_str),
            Some("1")
        );
        let receipt = serde_json::to_string(&receipt).unwrap();
        assert!(receipt.contains("managed-connect-proxy-session"));
        assert!(!receipt.contains("secret-token"));
    }

    #[test]
    fn mcp_context_is_independent_and_contains_no_provider_credentials() {
        let workspace = PathBuf::from("/tmp/hyper-mcp-context");
        let profile = SandboxProfile {
            enforcement: SandboxEnforcement::Native,
            filesystem: SandboxFileSystemPolicy::default(),
            network: SandboxNetworkPolicy::Offline,
            environment: SandboxEnvironmentPolicy::default(),
            platform: Default::default(),
            process: SandboxProcessPolicy::default(),
            resources: SandboxResourceLimits::default(),
            lifetime: SandboxLifetime::OneTask,
        };
        let (provider, _) = compile_agent_execution_context(
            Uuid::nil(),
            "codex-acp",
            &workspace,
            &BTreeMap::new(),
            profile.clone(),
            "http://127.0.0.1:43128",
            &[],
        )
        .unwrap();
        let (mcp, receipt) = compile_mcp_execution_context(
            Uuid::nil(),
            &workspace,
            workspace.join("mcp-home"),
            workspace.join("mcp-tmp"),
            &provider,
            profile,
        )
        .unwrap();

        assert_eq!(
            mcp.context_id,
            "mcp:hyper_term:00000000-0000-0000-0000-000000000000"
        );
        assert!(mcp.credential_bindings.is_empty());
        assert_eq!(
            mcp.environment.variables.get("HOME").map(String::as_str),
            Some("/tmp/hyper-mcp-context/mcp-home")
        );
        let serialized = serde_json::to_string(&receipt).unwrap();
        assert!(!serialized.contains("managed-connect-proxy"));
        assert!(!serialized.contains("HTTPS_PROXY"));
    }
}
