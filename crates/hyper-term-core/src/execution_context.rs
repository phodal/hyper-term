use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use hyper_term_protocol::{
    BindingLifetime, BindingScope, CollisionPolicy, ContextBindingReceipt, ContextDigest,
    ContextReceipt, CredentialRequirement, DerivedBinding, EXECUTION_CONTEXT_SCHEMA_VERSION,
    EnvironmentBindingOrigin, EnvironmentBindingSpec, EnvironmentClass, EnvironmentPlanDigest,
    EnvironmentSource, ExecutionContextSpec, ExecutionMode, OverridePolicy,
    ResolvedEnvironmentBinding, ResolvedEnvironmentPlan, ResolvedExecutionContext,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::canonicalize_sandbox_profile;

const MAX_CONTEXT_ID_BYTES: usize = 128;
const MAX_ENVIRONMENT_BINDINGS: usize = 128;
const MAX_ENVIRONMENT_BYTES: usize = 256 * 1024;
const MAX_ENV_FILE_BYTES: usize = 1024 * 1024;

const RESERVED_ENVIRONMENT_NAMES: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "HYPER_OPERATION_ID",
    "HYPER_CONTEXT_DIGEST",
    "HYPER_CONTROL_FD",
    "HYPER_CREDENTIAL_SOCKET",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecutionContextInputs {
    pub allow_user_mode: bool,
    pub host_environment: BTreeMap<String, String>,
    pub environment_files: BTreeMap<PathBuf, Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompiledBinding {
    value: Option<String>,
    metadata: ResolvedEnvironmentBinding,
    override_policy: OverridePolicy,
}

pub fn compile_execution_context(
    spec: &ExecutionContextSpec,
    inputs: &ExecutionContextInputs,
) -> Result<(ResolvedExecutionContext, ContextReceipt), ExecutionContextError> {
    let mut canonical = canonicalize_spec(spec, inputs.allow_user_mode)?;
    let mut compiled = BTreeMap::<String, CompiledBinding>::new();
    for binding in runtime_bindings(&canonical)? {
        insert_binding(&mut compiled, binding, CollisionPolicy::ExplicitOverride)?;
    }

    let mut bindings = canonical.environment.bindings.clone();
    bindings.sort_by(|left, right| {
        left.origin
            .cmp(&right.origin)
            .then_with(|| left.target_name.cmp(&right.target_name))
            .then_with(|| source_sort_key(&left.source).cmp(&source_sort_key(&right.source)))
    });
    canonical.environment.bindings = bindings.clone();
    for binding in bindings {
        validate_binding(&canonical, &binding)?;
        let value = resolve_binding_value(&canonical, &binding, inputs)?;
        let compiled_binding = CompiledBinding {
            value,
            metadata: ResolvedEnvironmentBinding {
                target_name: binding.target_name.clone(),
                class: binding.class,
                origin: binding.origin,
                scope: binding.scope,
                lifetime: binding.lifetime,
                source_kind: source_kind(&binding.source).into(),
            },
            override_policy: binding.override_policy,
        };
        insert_binding(
            &mut compiled,
            compiled_binding,
            canonical.environment.collision_policy,
        )?;
    }

    validate_credentials(&canonical.credentials)?;
    let variables = compiled
        .iter()
        .filter_map(|(name, binding)| {
            binding
                .value
                .as_ref()
                .map(|value| (name.clone(), value.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let total_bytes = variables.iter().try_fold(0_usize, |total, (name, value)| {
        total
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
    });
    if total_bytes.is_none_or(|total| total > MAX_ENVIRONMENT_BYTES) {
        return Err(ExecutionContextError::EnvironmentTooLarge);
    }
    let binding_metadata = compiled
        .into_values()
        .map(|binding| binding.metadata)
        .collect::<Vec<_>>();
    let clear_inherited = canonical.mode != ExecutionMode::User;
    let environment_digest = digest_environment(clear_inherited, &variables, &binding_metadata)?;
    if let Some(sandbox) = canonical.sandbox.as_mut() {
        sandbox.environment.clear_inherited = clear_inherited;
        sandbox.environment.variables = variables.clone();
        *sandbox = canonicalize_sandbox_profile(sandbox)
            .map_err(|error| ExecutionContextError::Sandbox(error.to_string()))?;
    }

    let context_digest = digest_context(&canonical, &environment_digest)?;
    let environment = ResolvedEnvironmentPlan {
        clear_inherited,
        variables,
        bindings: binding_metadata,
        digest: environment_digest.clone(),
    };
    let resolved = ResolvedExecutionContext {
        schema_version: canonical.schema_version,
        context_id: canonical.context_id.clone(),
        context_revision: canonical.context_revision,
        mode: canonical.mode,
        context_digest: context_digest.clone(),
        workspace: canonical.workspace.clone(),
        runtime: canonical.runtime.clone(),
        shell: canonical.shell.clone(),
        environment,
        credential_bindings: canonical.credentials.clone(),
        requested_sandbox: canonical.sandbox.clone(),
    };
    let receipt = ContextReceipt {
        schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
        context_id: canonical.context_id,
        context_revision: canonical.context_revision,
        mode: canonical.mode,
        context_digest,
        environment_digest,
        clear_inherited,
        bindings: resolved
            .environment
            .bindings
            .iter()
            .map(|binding| ContextBindingReceipt {
                target_name: binding.target_name.clone(),
                class: binding.class,
                origin: binding.origin,
                scope: binding.scope,
                lifetime: binding.lifetime,
                source_kind: binding.source_kind.clone(),
            })
            .collect(),
        credential_bindings: canonical.credentials,
    };
    Ok((resolved, receipt))
}

pub fn classify_environment_name(name: &str) -> EnvironmentClass {
    match name {
        "PATH" | "HOME" | "TMPDIR" | "LANG" | "LC_ALL" | "TZ" | "TERM" | "COLORTERM" => {
            EnvironmentClass::RuntimeSemantic
        }
        "BASH_ENV" | "ENV" | "ZDOTDIR" => EnvironmentClass::ShellStartup,
        "NODE_OPTIONS" | "PYTHONPATH" | "RUBYOPT" | "PERL5OPT" | "LD_PRELOAD" => {
            EnvironmentClass::LoaderInjection
        }
        "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY" | "WS_PROXY" | "WSS_PROXY"
        | "SSL_CERT_FILE" | "SSL_CERT_DIR" => EnvironmentClass::NetworkControl,
        "SSH_AUTH_SOCK" | "GPG_AGENT_INFO" | "DOCKER_HOST" | "KUBECONFIG" => {
            EnvironmentClass::AuthorityHandle
        }
        "TRACEPARENT" | "TRACESTATE" => EnvironmentClass::Observability,
        _ if name.starts_with("DYLD_") => EnvironmentClass::LoaderInjection,
        _ if name.starts_with("OTEL_") => EnvironmentClass::Observability,
        _ if name.ends_with("_TOKEN")
            || name.ends_with("_API_KEY")
            || name.ends_with("_SECRET")
            || name.ends_with("_PASSWORD") =>
        {
            EnvironmentClass::Credential
        }
        _ => EnvironmentClass::ToolConfiguration,
    }
}

fn canonicalize_spec(
    spec: &ExecutionContextSpec,
    allow_user_mode: bool,
) -> Result<ExecutionContextSpec, ExecutionContextError> {
    if spec.schema_version != EXECUTION_CONTEXT_SCHEMA_VERSION {
        return Err(ExecutionContextError::UnsupportedSchema(
            spec.schema_version,
        ));
    }
    if spec.context_revision == 0 {
        return Err(ExecutionContextError::ZeroRevision);
    }
    if spec.context_id.is_empty()
        || spec.context_id.len() > MAX_CONTEXT_ID_BYTES
        || !spec
            .context_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(ExecutionContextError::InvalidContextId);
    }
    if spec.mode == ExecutionMode::User && !allow_user_mode {
        return Err(ExecutionContextError::UserModeDenied);
    }
    if spec.environment.bindings.len() > MAX_ENVIRONMENT_BINDINGS {
        return Err(ExecutionContextError::TooManyBindings);
    }

    let mut canonical = spec.clone();
    canonical.workspace.root = normalize_absolute_path(&spec.workspace.root)?;
    canonical.workspace.working_directory =
        normalize_absolute_path(&spec.workspace.working_directory)?;
    canonical.workspace.runtime_home = normalize_absolute_path(&spec.workspace.runtime_home)?;
    canonical.workspace.runtime_temp = normalize_absolute_path(&spec.workspace.runtime_temp)?;
    if !canonical
        .workspace
        .working_directory
        .starts_with(&canonical.workspace.root)
    {
        return Err(ExecutionContextError::WorkingDirectoryOutsideWorkspace);
    }
    canonical.runtime.path.clear();
    let mut seen_runtime_paths = BTreeSet::new();
    for path in &spec.runtime.path {
        let path = normalize_absolute_path(path)?;
        if seen_runtime_paths.insert(path.clone()) {
            canonical.runtime.path.push(path);
        }
    }
    if canonical.runtime.path.is_empty() {
        return Err(ExecutionContextError::EmptyRuntimePath);
    }
    for (name, value) in [
        ("locale", &canonical.runtime.locale),
        ("timezone", &canonical.runtime.timezone),
        ("terminal", &canonical.runtime.terminal),
    ] {
        if value.is_empty() || value.contains('\0') {
            return Err(ExecutionContextError::InvalidRuntimeValue(name));
        }
    }
    if let Some(shell) = canonical.shell.as_mut() {
        shell.executable = normalize_absolute_path(&shell.executable)?;
        if shell.invocation.is_empty() || shell.invocation.contains('\0') {
            return Err(ExecutionContextError::InvalidShellInvocation);
        }
        if canonical.mode != ExecutionMode::User && shell.startup_files {
            return Err(ExecutionContextError::StartupFilesDenied);
        }
    }
    if canonical.mode != ExecutionMode::User && canonical.sandbox.is_none() {
        return Err(ExecutionContextError::SandboxRequired);
    }
    canonical.credentials.sort_by(|left, right| {
        left.binding_id
            .cmp(&right.binding_id)
            .then_with(|| left.target_name.cmp(&right.target_name))
    });
    Ok(canonical)
}

fn runtime_bindings(
    spec: &ExecutionContextSpec,
) -> Result<Vec<CompiledBinding>, ExecutionContextError> {
    let path = std::env::join_paths(&spec.runtime.path)
        .map_err(|_| ExecutionContextError::InvalidRuntimePath)?
        .into_string()
        .map_err(|_| ExecutionContextError::InvalidRuntimePath)?;
    let values = [
        ("PATH", path),
        ("HOME", utf8_path(&spec.workspace.runtime_home)?),
        ("TMPDIR", utf8_path(&spec.workspace.runtime_temp)?),
        ("LANG", spec.runtime.locale.clone()),
        ("TZ", spec.runtime.timezone.clone()),
        ("TERM", spec.runtime.terminal.clone()),
    ];
    Ok(values
        .into_iter()
        .map(|(target_name, value)| CompiledBinding {
            value: Some(value),
            metadata: ResolvedEnvironmentBinding {
                target_name: target_name.into(),
                class: EnvironmentClass::RuntimeSemantic,
                origin: EnvironmentBindingOrigin::RuntimeProfile,
                scope: BindingScope::ProcessTree,
                lifetime: BindingLifetime::Task,
                source_kind: "runtime_profile".into(),
            },
            override_policy: OverridePolicy::Deny,
        })
        .collect())
}

fn validate_binding(
    spec: &ExecutionContextSpec,
    binding: &EnvironmentBindingSpec,
) -> Result<(), ExecutionContextError> {
    validate_environment_name(&binding.target_name)?;
    let inferred = classify_environment_name(&binding.target_name);
    if inferred != EnvironmentClass::ToolConfiguration && inferred != binding.class {
        return Err(ExecutionContextError::EnvironmentClassMismatch {
            name: binding.target_name.clone(),
            expected: inferred,
            actual: binding.class,
        });
    }
    if RESERVED_ENVIRONMENT_NAMES.contains(&binding.target_name.as_str())
        && binding.origin != EnvironmentBindingOrigin::Authority
        && binding.origin != EnvironmentBindingOrigin::CredentialBroker
    {
        return Err(ExecutionContextError::ReservedEnvironmentName(
            binding.target_name.clone(),
        ));
    }
    if binding.origin == EnvironmentBindingOrigin::Workspace {
        if spec.mode != ExecutionMode::Project {
            return Err(ExecutionContextError::WorkspaceBindingOutsideProject);
        }
        if !matches!(
            binding.class,
            EnvironmentClass::ToolConfiguration | EnvironmentClass::Observability
        ) {
            return Err(ExecutionContextError::WorkspaceAuthorityBinding(
                binding.target_name.clone(),
            ));
        }
    }
    if spec.mode != ExecutionMode::User
        && matches!(
            binding.class,
            EnvironmentClass::ShellStartup | EnvironmentClass::LoaderInjection
        )
    {
        return Err(ExecutionContextError::CodeInjectionBinding(
            binding.target_name.clone(),
        ));
    }
    if binding.class == EnvironmentClass::NetworkControl
        && binding.origin != EnvironmentBindingOrigin::Authority
    {
        return Err(ExecutionContextError::NetworkBindingRequiresAuthority(
            binding.target_name.clone(),
        ));
    }
    if binding.class == EnvironmentClass::AuthorityHandle
        && !matches!(
            binding.origin,
            EnvironmentBindingOrigin::Authority | EnvironmentBindingOrigin::CredentialBroker
        )
    {
        return Err(ExecutionContextError::AuthorityHandleRequiresBroker(
            binding.target_name.clone(),
        ));
    }
    match (&binding.source, binding.class) {
        (EnvironmentSource::Literal { .. }, EnvironmentClass::Credential) => Err(
            ExecutionContextError::CredentialLiteral(binding.target_name.clone()),
        ),
        (EnvironmentSource::SecretReference { .. }, EnvironmentClass::Credential) => Ok(()),
        (EnvironmentSource::SecretReference { .. }, _) => Err(
            ExecutionContextError::SecretReferenceRequiresCredential(binding.target_name.clone()),
        ),
        (EnvironmentSource::HostVariable { .. }, _) if spec.mode != ExecutionMode::User => Err(
            ExecutionContextError::HostVariableDenied(binding.target_name.clone()),
        ),
        _ => Ok(()),
    }
}

fn resolve_binding_value(
    spec: &ExecutionContextSpec,
    binding: &EnvironmentBindingSpec,
    inputs: &ExecutionContextInputs,
) -> Result<Option<String>, ExecutionContextError> {
    let value = match &binding.source {
        EnvironmentSource::Literal { value } => value.clone(),
        EnvironmentSource::HostVariable { name } => {
            validate_environment_name(name)?;
            inputs
                .host_environment
                .get(name)
                .cloned()
                .ok_or_else(|| ExecutionContextError::MissingHostVariable(name.clone()))?
        }
        EnvironmentSource::EnvFileKey {
            path,
            key,
            expected_sha256,
        } => resolve_env_file(spec, inputs, path, key, expected_sha256.as_deref())?,
        EnvironmentSource::SecretReference { .. } => return Ok(None),
        EnvironmentSource::Derived { binding } => match binding {
            DerivedBinding::WorkspaceRoot => utf8_path(&spec.workspace.root)?,
            DerivedBinding::WorkingDirectory => utf8_path(&spec.workspace.working_directory)?,
            DerivedBinding::RuntimeHome => utf8_path(&spec.workspace.runtime_home)?,
            DerivedBinding::RuntimeTemp => utf8_path(&spec.workspace.runtime_temp)?,
        },
    };
    if value.contains('\0') {
        return Err(ExecutionContextError::NulEnvironmentValue(
            binding.target_name.clone(),
        ));
    }
    Ok(Some(value))
}

fn resolve_env_file(
    spec: &ExecutionContextSpec,
    inputs: &ExecutionContextInputs,
    path: &Path,
    key: &str,
    expected_sha256: Option<&str>,
) -> Result<String, ExecutionContextError> {
    validate_environment_name(key)?;
    let path = if path.is_absolute() {
        normalize_absolute_path(path)?
    } else {
        normalize_absolute_path(&spec.workspace.root.join(path))?
    };
    if !path.starts_with(&spec.workspace.root) {
        return Err(ExecutionContextError::EnvironmentFileOutsideWorkspace(path));
    }
    let bytes = inputs
        .environment_files
        .get(&path)
        .ok_or_else(|| ExecutionContextError::EnvironmentFileMissing(path.clone()))?;
    if bytes.len() > MAX_ENV_FILE_BYTES {
        return Err(ExecutionContextError::EnvironmentFileTooLarge(path));
    }
    if let Some(expected) = expected_sha256 {
        if !valid_sha256(expected) {
            return Err(ExecutionContextError::InvalidEnvironmentFileDigest);
        }
        let actual = hex_digest(bytes);
        if actual != expected {
            return Err(ExecutionContextError::EnvironmentFileDigestMismatch {
                expected: expected.into(),
                actual,
            });
        }
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ExecutionContextError::EnvironmentFileNotUtf8(path.clone()))?;
    parse_env_file_value(text, key)
}

fn parse_env_file_value(
    contents: &str,
    requested_key: &str,
) -> Result<String, ExecutionContextError> {
    let mut values = BTreeMap::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("export ") {
            return Err(ExecutionContextError::UnsupportedEnvironmentFileSyntax(
                index + 1,
            ));
        }
        let (name, raw_value) = line
            .split_once('=')
            .ok_or(ExecutionContextError::MalformedEnvironmentFile(index + 1))?;
        validate_environment_name(name)
            .map_err(|_| ExecutionContextError::MalformedEnvironmentFile(index + 1))?;
        if values.contains_key(name) {
            return Err(ExecutionContextError::DuplicateEnvironmentFileKey(
                name.into(),
            ));
        }
        let value = parse_env_file_scalar(raw_value, index + 1)?;
        values.insert(name.to_owned(), value);
    }
    values
        .remove(requested_key)
        .ok_or_else(|| ExecutionContextError::EnvironmentFileKeyMissing(requested_key.into()))
}

fn parse_env_file_scalar(value: &str, line: usize) -> Result<String, ExecutionContextError> {
    if value.contains('\0') {
        return Err(ExecutionContextError::MalformedEnvironmentFile(line));
    }
    let Some(quote) = value.as_bytes().first().copied() else {
        return Ok(String::new());
    };
    if quote != b'\'' && quote != b'"' {
        if value.trim() != value || value.contains('\\') {
            return Err(ExecutionContextError::UnsupportedEnvironmentFileSyntax(
                line,
            ));
        }
        return Ok(value.to_owned());
    }
    if value.len() < 2 || value.as_bytes().last().copied() != Some(quote) {
        return Err(ExecutionContextError::MalformedEnvironmentFile(line));
    }
    let inner = &value[1..value.len() - 1];
    if inner.as_bytes().contains(&quote) || inner.contains('\\') {
        return Err(ExecutionContextError::UnsupportedEnvironmentFileSyntax(
            line,
        ));
    }
    Ok(inner.to_owned())
}

fn insert_binding(
    compiled: &mut BTreeMap<String, CompiledBinding>,
    binding: CompiledBinding,
    collision_policy: CollisionPolicy,
) -> Result<(), ExecutionContextError> {
    let name = binding.metadata.target_name.clone();
    let Some(existing) = compiled.get(&name) else {
        compiled.insert(name, binding);
        return Ok(());
    };
    let replace = collision_policy == CollisionPolicy::ExplicitOverride
        && binding.override_policy == OverridePolicy::ReplaceSameClass
        && existing.metadata.class == binding.metadata.class
        && binding.metadata.origin > existing.metadata.origin;
    if !replace {
        return Err(ExecutionContextError::EnvironmentCollision(name));
    }
    compiled.insert(name, binding);
    Ok(())
}

fn validate_credentials(
    credentials: &[CredentialRequirement],
) -> Result<(), ExecutionContextError> {
    let mut ids = BTreeSet::new();
    for credential in credentials {
        if credential.binding_id.is_empty()
            || credential.target_name.is_empty()
            || credential.audience.is_empty()
            || credential.reference.provider_id.is_empty()
            || credential.reference.secret_id.is_empty()
        {
            return Err(ExecutionContextError::InvalidCredentialRequirement);
        }
        if !ids.insert(&credential.binding_id) {
            return Err(ExecutionContextError::DuplicateCredentialBinding(
                credential.binding_id.clone(),
            ));
        }
    }
    Ok(())
}

fn digest_environment(
    clear_inherited: bool,
    variables: &BTreeMap<String, String>,
    bindings: &[ResolvedEnvironmentBinding],
) -> Result<EnvironmentPlanDigest, ExecutionContextError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        clear_inherited: bool,
        variables: &'a BTreeMap<String, String>,
        bindings: &'a [ResolvedEnvironmentBinding],
    }
    EnvironmentPlanDigest::parse(sha256_json(&DigestInput {
        clear_inherited,
        variables,
        bindings,
    })?)
    .map_err(|error| ExecutionContextError::Digest(error.to_string()))
}

fn digest_context(
    spec: &ExecutionContextSpec,
    environment_digest: &EnvironmentPlanDigest,
) -> Result<ContextDigest, ExecutionContextError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        spec: &'a ExecutionContextSpec,
        environment_digest: &'a EnvironmentPlanDigest,
    }
    ContextDigest::parse(sha256_json(&DigestInput {
        spec,
        environment_digest,
    })?)
    .map_err(|error| ExecutionContextError::Digest(error.to_string()))
}

fn source_kind(source: &EnvironmentSource) -> &'static str {
    match source {
        EnvironmentSource::Literal { .. } => "literal",
        EnvironmentSource::HostVariable { .. } => "host_variable",
        EnvironmentSource::EnvFileKey { .. } => "env_file_key",
        EnvironmentSource::SecretReference { .. } => "secret_reference",
        EnvironmentSource::Derived { .. } => "derived",
    }
}

fn source_sort_key(source: &EnvironmentSource) -> String {
    serde_json::to_string(source).unwrap_or_default()
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf, ExecutionContextError> {
    if !path.is_absolute() {
        return Err(ExecutionContextError::PathMustBeAbsolute(
            path.to_path_buf(),
        ));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ExecutionContextError::PathEscapesRoot(path.to_path_buf()));
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Ok(normalized)
}

fn validate_environment_name(name: &str) -> Result<(), ExecutionContextError> {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return Err(ExecutionContextError::InvalidEnvironmentName(name.into()));
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ExecutionContextError::InvalidEnvironmentName(name.into()));
    }
    Ok(())
}

fn utf8_path(path: &Path) -> Result<String, ExecutionContextError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(ExecutionContextError::NonUtf8Path)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_json(value: &impl Serialize) -> Result<String, ExecutionContextError> {
    Ok(hex_digest(&serde_json::to_vec(value)?))
}

#[derive(Debug, Error)]
pub enum ExecutionContextError {
    #[error("unsupported execution-context schema {0}")]
    UnsupportedSchema(u16),
    #[error("execution context revision must be non-zero")]
    ZeroRevision,
    #[error("execution context ID is invalid")]
    InvalidContextId,
    #[error("user execution mode is not available through this caller")]
    UserModeDenied,
    #[error("execution context has too many environment bindings")]
    TooManyBindings,
    #[error("execution context environment exceeds its byte budget")]
    EnvironmentTooLarge,
    #[error("execution context path must be absolute: {0}")]
    PathMustBeAbsolute(PathBuf),
    #[error("execution context path escapes its root: {0}")]
    PathEscapesRoot(PathBuf),
    #[error("execution context path is not UTF-8")]
    NonUtf8Path,
    #[error("working directory is outside the workspace")]
    WorkingDirectoryOutsideWorkspace,
    #[error("runtime PATH must contain at least one absolute directory")]
    EmptyRuntimePath,
    #[error("runtime PATH cannot be represented safely")]
    InvalidRuntimePath,
    #[error("runtime {0} is invalid")]
    InvalidRuntimeValue(&'static str),
    #[error("shell invocation is invalid")]
    InvalidShellInvocation,
    #[error("shell startup files are denied outside user mode")]
    StartupFilesDenied,
    #[error("hermetic and project execution contexts require a sandbox")]
    SandboxRequired,
    #[error("invalid environment name {0}")]
    InvalidEnvironmentName(String),
    #[error("environment {name} is classified as {expected:?}, not {actual:?}")]
    EnvironmentClassMismatch {
        name: String,
        expected: EnvironmentClass,
        actual: EnvironmentClass,
    },
    #[error("environment name {0} is reserved for Rust authority")]
    ReservedEnvironmentName(String),
    #[error("workspace environment bindings require project mode")]
    WorkspaceBindingOutsideProject,
    #[error("workspace binding {0} requests machine authority")]
    WorkspaceAuthorityBinding(String),
    #[error("code-injection environment binding {0} is denied")]
    CodeInjectionBinding(String),
    #[error("network environment binding {0} requires Rust authority")]
    NetworkBindingRequiresAuthority(String),
    #[error("authority handle {0} requires a broker")]
    AuthorityHandleRequiresBroker(String),
    #[error("credential-class binding {0} cannot contain a literal")]
    CredentialLiteral(String),
    #[error("secret reference {0} must be credential-class")]
    SecretReferenceRequiresCredential(String),
    #[error("host variable {0} is denied outside user mode")]
    HostVariableDenied(String),
    #[error("host variable {0} is missing")]
    MissingHostVariable(String),
    #[error("environment value {0} contains NUL")]
    NulEnvironmentValue(String),
    #[error("environment binding collision for {0}")]
    EnvironmentCollision(String),
    #[error("environment file is outside the workspace: {0}")]
    EnvironmentFileOutsideWorkspace(PathBuf),
    #[error("environment file input is missing: {0}")]
    EnvironmentFileMissing(PathBuf),
    #[error("environment file exceeds its byte budget: {0}")]
    EnvironmentFileTooLarge(PathBuf),
    #[error("environment file is not UTF-8: {0}")]
    EnvironmentFileNotUtf8(PathBuf),
    #[error("environment file SHA-256 is invalid")]
    InvalidEnvironmentFileDigest,
    #[error("environment file digest mismatch: expected {expected}, got {actual}")]
    EnvironmentFileDigestMismatch { expected: String, actual: String },
    #[error("environment file line {0} is malformed")]
    MalformedEnvironmentFile(usize),
    #[error("environment file line {0} uses unsupported syntax")]
    UnsupportedEnvironmentFileSyntax(usize),
    #[error("environment file contains duplicate key {0}")]
    DuplicateEnvironmentFileKey(String),
    #[error("environment file key {0} is missing")]
    EnvironmentFileKeyMissing(String),
    #[error("credential requirement is invalid")]
    InvalidCredentialRequirement,
    #[error("credential binding {0} is duplicated")]
    DuplicateCredentialBinding(String),
    #[error("sandbox context is invalid: {0}")]
    Sandbox(String),
    #[error("execution-context digest failed: {0}")]
    Digest(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use hyper_term_protocol::{
        EnvironmentPlan, RuntimeEnvironmentSpec, SandboxEnforcement, SandboxEnvironmentPolicy,
        SandboxFileSystemPolicy, SandboxLifetime, SandboxNetworkPolicy, SandboxProcessPolicy,
        SandboxProfile, SandboxResourceLimits, WorkspaceContextSpec,
    };

    use super::*;

    fn spec(root: &Path) -> ExecutionContextSpec {
        ExecutionContextSpec {
            schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
            context_id: "agent:fixture".into(),
            context_revision: 1,
            mode: ExecutionMode::Hermetic,
            workspace: WorkspaceContextSpec {
                root: root.into(),
                working_directory: root.into(),
                runtime_home: root.join("home"),
                runtime_temp: root.join("tmp"),
            },
            runtime: RuntimeEnvironmentSpec {
                path: vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")],
                locale: "C.UTF-8".into(),
                timezone: "UTC".into(),
                terminal: "xterm-256color".into(),
            },
            shell: None,
            environment: EnvironmentPlan::default(),
            credentials: Vec::new(),
            sandbox: Some(SandboxProfile {
                enforcement: SandboxEnforcement::Native,
                filesystem: SandboxFileSystemPolicy::default(),
                network: SandboxNetworkPolicy::Offline,
                environment: SandboxEnvironmentPolicy::default(),
                process: SandboxProcessPolicy::default(),
                resources: SandboxResourceLimits::default(),
                lifetime: SandboxLifetime::OneTask,
            }),
        }
    }

    #[test]
    fn canonical_order_produces_stable_context_and_environment_digests() {
        let root = PathBuf::from("/tmp/hyper-context");
        let mut left = spec(&root);
        left.environment.collision_policy = CollisionPolicy::ExplicitOverride;
        left.environment.bindings = vec![
            literal("MODEL", "gpt-test", EnvironmentBindingOrigin::Invocation),
            literal("LOG_LEVEL", "debug", EnvironmentBindingOrigin::Invocation),
        ];
        let mut right = left.clone();
        right.environment.bindings.reverse();

        let (left, left_receipt) =
            compile_execution_context(&left, &ExecutionContextInputs::default()).unwrap();
        let (right, right_receipt) =
            compile_execution_context(&right, &ExecutionContextInputs::default()).unwrap();
        assert_eq!(left.context_digest, right.context_digest);
        assert_eq!(left.environment.digest, right.environment.digest);
        assert_eq!(left_receipt, right_receipt);
    }

    #[test]
    fn hermetic_context_rejects_startup_loader_and_host_inheritance() {
        let root = PathBuf::from("/tmp/hyper-context");
        for binding in [
            literal(
                "BASH_ENV",
                "/tmp/startup",
                EnvironmentBindingOrigin::Invocation,
            ),
            literal(
                "DYLD_INSERT_LIBRARIES",
                "/tmp/hook",
                EnvironmentBindingOrigin::Invocation,
            ),
            EnvironmentBindingSpec {
                target_name: "MODEL".into(),
                source: EnvironmentSource::HostVariable {
                    name: "MODEL".into(),
                },
                class: EnvironmentClass::ToolConfiguration,
                origin: EnvironmentBindingOrigin::Invocation,
                scope: BindingScope::ProcessTree,
                lifetime: BindingLifetime::Task,
                override_policy: OverridePolicy::Deny,
            },
        ] {
            let mut candidate = spec(&root);
            candidate.environment.bindings.push(binding);
            assert!(
                compile_execution_context(&candidate, &ExecutionContextInputs::default()).is_err()
            );
        }
    }

    #[test]
    fn project_env_file_is_key_scoped_digest_pinned_and_redacted_from_receipt() {
        let root = PathBuf::from("/tmp/hyper-context");
        let path = root.join(".env.agent");
        let bytes = b"LOG_LEVEL=debug\nUNUSED=private\n".to_vec();
        let mut candidate = spec(&root);
        candidate.mode = ExecutionMode::Project;
        candidate.environment.bindings.push(EnvironmentBindingSpec {
            target_name: "LOG_LEVEL".into(),
            source: EnvironmentSource::EnvFileKey {
                path: PathBuf::from(".env.agent"),
                key: "LOG_LEVEL".into(),
                expected_sha256: Some(hex_digest(&bytes)),
            },
            class: EnvironmentClass::ToolConfiguration,
            origin: EnvironmentBindingOrigin::Workspace,
            scope: BindingScope::ProcessTree,
            lifetime: BindingLifetime::Task,
            override_policy: OverridePolicy::Deny,
        });
        let inputs = ExecutionContextInputs {
            environment_files: BTreeMap::from([(path, bytes)]),
            ..ExecutionContextInputs::default()
        };
        let (resolved, receipt) = compile_execution_context(&candidate, &inputs).unwrap();
        assert_eq!(
            resolved
                .environment
                .variables
                .get("LOG_LEVEL")
                .map(String::as_str),
            Some("debug")
        );
        let receipt = serde_json::to_string(&receipt).unwrap();
        assert!(!receipt.contains("debug"));
        assert!(!receipt.contains("private"));
        assert!(receipt.contains("env_file_key"));
    }

    #[test]
    fn credentials_are_reference_only_and_never_materialized() {
        let root = PathBuf::from("/tmp/hyper-context");
        let mut candidate = spec(&root);
        candidate.credentials.push(CredentialRequirement {
            binding_id: "managed-proxy".into(),
            reference: hyper_term_protocol::SecretReference {
                provider_id: "daemon".into(),
                secret_id: "proxy-session".into(),
                version: Some("1".into()),
            },
            target_name: "HTTPS_PROXY".into(),
            audience: "managed-connect-proxy".into(),
            scope: BindingScope::ProcessTree,
            lifetime: BindingLifetime::Task,
        });
        let (without_credentials, _) =
            compile_execution_context(&spec(&root), &ExecutionContextInputs::default()).unwrap();
        let (resolved, receipt) =
            compile_execution_context(&candidate, &ExecutionContextInputs::default()).unwrap();
        assert!(!resolved.environment.variables.contains_key("HTTPS_PROXY"));
        assert_eq!(
            resolved.environment.digest,
            without_credentials.environment.digest
        );
        assert_ne!(resolved.context_digest, without_credentials.context_digest);
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(json.contains("proxy-session"));
        assert!(!json.contains("secret-token"));
    }

    #[test]
    fn reserved_and_credential_literals_fail_closed() {
        let root = PathBuf::from("/tmp/hyper-context");
        let mut reserved = spec(&root);
        reserved.environment.bindings.push(literal(
            "HYPER_OPERATION_ID",
            "forged",
            EnvironmentBindingOrigin::Invocation,
        ));
        assert!(matches!(
            compile_execution_context(&reserved, &ExecutionContextInputs::default()),
            Err(ExecutionContextError::ReservedEnvironmentName(_))
        ));

        let mut credential = spec(&root);
        let mut binding = literal(
            "OPENAI_API_KEY",
            "secret-token",
            EnvironmentBindingOrigin::Invocation,
        );
        binding.class = EnvironmentClass::Credential;
        credential.environment.bindings.push(binding);
        assert!(matches!(
            compile_execution_context(&credential, &ExecutionContextInputs::default()),
            Err(ExecutionContextError::CredentialLiteral(_))
        ));
    }

    fn literal(
        name: &str,
        value: &str,
        origin: EnvironmentBindingOrigin,
    ) -> EnvironmentBindingSpec {
        EnvironmentBindingSpec {
            target_name: name.into(),
            source: EnvironmentSource::Literal {
                value: value.into(),
            },
            class: classify_environment_name(name),
            origin,
            scope: BindingScope::ProcessTree,
            lifetime: BindingLifetime::Task,
            override_policy: OverridePolicy::Deny,
        }
    }
}
