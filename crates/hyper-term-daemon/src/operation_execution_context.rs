use std::path::{Path, PathBuf};

use hyper_term_core::classify_environment_name;
use hyper_term_protocol::{
    BindingLifetime, BindingScope, CollisionPolicy, EXECUTION_CONTEXT_SCHEMA_VERSION,
    EnvironmentBindingOrigin, EnvironmentBindingSpec, EnvironmentPlan, EnvironmentSource,
    ExecutionContextSpec, ExecutionMode, OperationId, OverridePolicy, RuntimeEnvironmentSpec,
    SandboxProfile, ShellContextSpec, TerminalCommand, WorkspaceContextSpec,
};

pub(super) fn operation_execution_context_spec(
    operation_id: OperationId,
    operation_revision: u64,
    command: &TerminalCommand,
    workspace: &Path,
    runtime_root: &Path,
    sandbox: SandboxProfile,
) -> ExecutionContextSpec {
    let bindings = command
        .env
        .iter()
        .map(|(name, value)| EnvironmentBindingSpec {
            target_name: name.clone(),
            source: EnvironmentSource::Literal {
                value: value.clone(),
            },
            class: classify_environment_name(name),
            origin: EnvironmentBindingOrigin::Invocation,
            scope: BindingScope::ProcessTree,
            lifetime: BindingLifetime::Operation,
            override_policy: OverridePolicy::ReplaceSameClass,
        })
        .collect();
    ExecutionContextSpec {
        schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
        context_id: format!("operation:{operation_id}"),
        context_revision: operation_revision,
        mode: ExecutionMode::Hermetic,
        workspace: WorkspaceContextSpec {
            root: workspace.to_path_buf(),
            working_directory: workspace.to_path_buf(),
            runtime_home: runtime_root.to_path_buf(),
            runtime_temp: runtime_root.to_path_buf(),
        },
        runtime: RuntimeEnvironmentSpec {
            path: ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            locale: "C.UTF-8".into(),
            timezone: "UTC".into(),
            terminal: "xterm-256color".into(),
        },
        shell: Some(ShellContextSpec {
            executable: PathBuf::from(&command.program),
            invocation: "pty_command".into(),
            startup_files: false,
            pty: true,
        }),
        environment: EnvironmentPlan {
            bindings,
            collision_policy: CollisionPolicy::ExplicitOverride,
        },
        credentials: Vec::new(),
        sandbox: Some(sandbox),
    }
}
