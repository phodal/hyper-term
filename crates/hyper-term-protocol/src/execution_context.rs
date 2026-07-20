use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ContextDigest, EnvironmentPlanDigest, SandboxProfile};

pub const EXECUTION_CONTEXT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Hermetic,
    Project,
    User,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentBindingOrigin {
    BuiltIn,
    RuntimeProfile,
    Workspace,
    Invocation,
    Authority,
    CredentialBroker,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentClass {
    RuntimeSemantic,
    ShellStartup,
    LoaderInjection,
    NetworkControl,
    AuthorityHandle,
    Credential,
    ToolConfiguration,
    Observability,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingScope {
    Process,
    ProcessTree,
    Operation,
    Task,
    Server,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingLifetime {
    Launch,
    Operation,
    Task,
    Server,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverridePolicy {
    Deny,
    ReplaceSameClass,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    Deny,
    ExplicitOverride,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SecretReference {
    pub provider_id: String,
    pub secret_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedBinding {
    WorkspaceRoot,
    WorkingDirectory,
    RuntimeHome,
    RuntimeTemp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvironmentSource {
    Literal {
        value: String,
    },
    HostVariable {
        name: String,
    },
    EnvFileKey {
        path: PathBuf,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_sha256: Option<String>,
    },
    SecretReference {
        reference: SecretReference,
    },
    Derived {
        binding: DerivedBinding,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvironmentBindingSpec {
    pub target_name: String,
    pub source: EnvironmentSource,
    pub class: EnvironmentClass,
    pub origin: EnvironmentBindingOrigin,
    pub scope: BindingScope,
    pub lifetime: BindingLifetime,
    pub override_policy: OverridePolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvironmentPlan {
    #[serde(default)]
    pub bindings: Vec<EnvironmentBindingSpec>,
    pub collision_policy: CollisionPolicy,
}

impl Default for EnvironmentPlan {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
            collision_policy: CollisionPolicy::Deny,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceContextSpec {
    pub root: PathBuf,
    pub working_directory: PathBuf,
    pub runtime_home: PathBuf,
    pub runtime_temp: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeEnvironmentSpec {
    #[serde(default)]
    pub path: Vec<PathBuf>,
    pub locale: String,
    pub timezone: String,
    pub terminal: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShellContextSpec {
    pub executable: PathBuf,
    pub invocation: String,
    pub startup_files: bool,
    pub pty: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CredentialRequirement {
    pub binding_id: String,
    pub reference: SecretReference,
    pub target_name: String,
    pub audience: String,
    pub scope: BindingScope,
    pub lifetime: BindingLifetime,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionContextSpec {
    pub schema_version: u16,
    pub context_id: String,
    pub context_revision: u64,
    pub mode: ExecutionMode,
    pub workspace: WorkspaceContextSpec,
    pub runtime: RuntimeEnvironmentSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellContextSpec>,
    #[serde(default)]
    pub environment: EnvironmentPlan,
    #[serde(default)]
    pub credentials: Vec<CredentialRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedEnvironmentBinding {
    pub target_name: String,
    pub class: EnvironmentClass,
    pub origin: EnvironmentBindingOrigin,
    pub scope: BindingScope,
    pub lifetime: BindingLifetime,
    pub source_kind: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedEnvironmentPlan {
    pub clear_inherited: bool,
    pub variables: BTreeMap<String, String>,
    pub bindings: Vec<ResolvedEnvironmentBinding>,
    pub digest: EnvironmentPlanDigest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedExecutionContext {
    pub schema_version: u16,
    pub context_id: String,
    pub context_revision: u64,
    pub mode: ExecutionMode,
    pub context_digest: ContextDigest,
    pub workspace: WorkspaceContextSpec,
    pub runtime: RuntimeEnvironmentSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellContextSpec>,
    pub environment: ResolvedEnvironmentPlan,
    #[serde(default)]
    pub credential_bindings: Vec<CredentialRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_sandbox: Option<SandboxProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextBindingReceipt {
    pub target_name: String,
    pub class: EnvironmentClass,
    pub origin: EnvironmentBindingOrigin,
    pub scope: BindingScope,
    pub lifetime: BindingLifetime,
    pub source_kind: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextReceipt {
    pub schema_version: u16,
    pub context_id: String,
    pub context_revision: u64,
    pub mode: ExecutionMode,
    pub context_digest: ContextDigest,
    pub environment_digest: EnvironmentPlanDigest,
    pub clear_inherited: bool,
    #[serde(default)]
    pub bindings: Vec<ContextBindingReceipt>,
    #[serde(default)]
    pub credential_bindings: Vec<CredentialRequirement>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentExecutionContextReceiptSet {
    pub provider_id: String,
    pub protocol: String,
    pub thread_id: String,
    pub receipts: Vec<ContextReceipt>,
}
