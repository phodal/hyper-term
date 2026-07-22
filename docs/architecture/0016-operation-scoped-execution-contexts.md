# ADR 0016: Compile execution contexts into operation-scoped authority

- Status: proposed
- Date: 2026-07-20
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md),
  [ADR 0009](0009-rust-acp-mcp-adapters.md), and
  [ADR 0014](0014-rust-owned-coding-agent-sandbox.md)
- Refines: environment, credential, shell-startup, and MCP identity handling in
  [ADR 0009](0009-rust-acp-mcp-adapters.md) and
  [ADR 0014](0014-rust-owned-coding-agent-sandbox.md)

## Context

Hyper Term starts several kinds of execution with different wire protocols but
overlapping machine authority:

- an ordinary interactive Bash or Zsh terminal;
- an Agent-owned shell operation;
- a local ACP agent or direct provider adapter;
- a local stdio MCP server and its descendants;
- a remote HTTP MCP connection;
- a supervised compiler, language server, test runner, or generated program;
- a future headless, IDE, or CI consumer of the Rust daemon.

The Rust kernel already owns process lifecycle, executable verification,
operation revisions, permission decisions, one-use capability leases, sandbox
profiles, and durable receipts. Driver processes clear the inherited
environment and receive an explicit environment map. Sandbox compilation binds
an exact command and policy digest before launch.

Those are strong authority primitives, but the current environment model is
still a final-value map:

```rust
pub struct SandboxEnvironmentPolicy {
    pub clear_inherited: bool,
    pub variables: BTreeMap<String, String>,
}
```

Driver configuration similarly stores:

```rust
pub environment: BTreeMap<String, OsString>
```

These maps can express which bytes reach a process, but not:

- where a value came from;
- whether it is ordinary configuration, executable startup behavior, an
  authority handle, or a credential;
- who may receive it and which descendants may inherit it;
- whether it is valid for one process, one operation, one task, or a complete
  server lifetime;
- whether it may be serialized, logged, displayed, hashed, or retained;
- whether it is delivered through the environment, a private file, an
  inherited descriptor, a broker socket, an HTTP header, or a host-mediated
  operation;
- which approval, executable, MCP server, tool contract, workspace snapshot,
  network audience, and operation revision authorized it.

Environment values are not passive data. `PATH`, `HOME`, `TMPDIR`, locale, and
shell startup variables change runtime semantics. Loader and interpreter
variables such as `BASH_ENV`, `ENV`, `ZDOTDIR`, `NODE_OPTIONS`, `PYTHONPATH`,
`LD_PRELOAD`, and `DYLD_*` can change which code executes. Socket and file
variables such as `SSH_AUTH_SOCK`, `DOCKER_HOST`, and `KUBECONFIG` can grant
machine authority. Tokens and API keys are credentials even when legacy tools
receive them through the same environment channel.

MCP makes this distinction urgent. A local stdio MCP server is a child process
started by the client and commonly obtains local credentials from its launch
environment. A long-lived server that receives a broad token at startup holds
that token before any individual tool call is approved. Per-call UI approval
does not retroactively narrow the server process, its descendants, or the
credential it already owns.

The current Hyper Term MCP implementation is intentionally narrower than a
general MCP host. It exposes a bounded, built-in northbound stdio server for
Diff, Deno LSP, and GenUI. Each call becomes an operation proposal and is
authorized by Rust before execution. It does not yet supervise arbitrary local
MCP packages, proxy arbitrary southbound servers, or manage remote MCP OAuth.
Those are new authority surfaces, not configuration-only additions.

Simple `.env` loading and MCP configuration are already commodity client
features. Hyper Term's durable boundary must instead answer:

> Which exact execution identity received which authority, from which source,
> for which operation and lifetime, under which sandbox, and with which durable
> evidence?

## Decision drivers

1. Rust remains the sole machine-authority, credential-resolution, approval,
   and process-dispatch boundary.
2. An execution context describes and compiles authority; it is not itself a
   reusable bearer capability.
3. The immutable `Operation` revision and one-use `CapabilityLease` remain the
   unit of authorization.
4. Ordinary user terminals retain the user's real shell semantics and do not
   silently become Agent or MCP execution capabilities.
5. Secret values never enter serializable protocol objects, workspace config,
   journal payloads, renderer state, ordinary action digests, or diagnostic
   formatting.
6. Local processes and remote services use one effect and evidence model but
   make different enforcement claims.
7. MCP server launch and MCP tool invocation are separately authorized effects.
8. A tool name, annotation, root, model-provided risk label, or MCP approval
   prompt is never an operating-system security boundary.
9. Existing `hyper-term-protocol`, `hyper-term-core`, `hyper-term-sandbox`,
   `hyper-term-drivers`, and `hyper-term-daemon` ownership remains intact until
   a real second product consumer proves that extraction is useful.
10. Context compilation, collision handling, secret resolution, dispatch,
    expiry, cancellation, violation, and receipt generation are deterministic
    and testable without a renderer.
11. Protocol version and extension negotiation are explicit because MCP wire
    behavior evolves independently of Hyper Term's durable operation model.

## Terminology

This ADR uses five deliberately separate representations.

### `ExecutionContextSpec`

A declarative, serializable request for execution semantics and authority. It
contains references and requirements, never resolved secret bytes. It may be
constructed from built-in policy, trusted user configuration, bounded
workspace configuration, or a typed API request.

### `ResolvedExecutionContext`

The canonical result of validation, layering, static identity resolution, and
policy-input collection. It contains normalized paths, executable and package
identities, environment provenance, credential requirements, requested sandbox
inputs, negotiated protocol information, and digests. It still does not contain
raw secret bytes and does not authorize dispatch.

### `AuthorizedExecutionPlan`

An immutable plan for one exact authorized `Operation` revision. It binds the
resolved context to the compiled sandbox or remote enforcement boundary,
credential-lease metadata, action digest, budgets, actor, expiry, and one-use
capability lease. Possessing an unbound resolved context is insufficient to
construct this plan.

### `MaterializedLaunch`

An opaque daemon-owned object created only after exact operation authorization.
It may briefly hold secret values or handles needed to create one process or
one remote request. It is not `Serialize`, is not `Debug`, is not sent over the
control protocol, and is consumed at dispatch.

### `ContextReceipt`

A serializable, redacted account of what was compiled and enforced. It records
identities, references, scopes, audiences, exposure methods, lifetimes,
digests, and outcomes, but never secret values.

`SandboxProfile` and `CompiledSandboxProfile` remain the operating-system policy
and enforcement projections inside the resolved context and authorized plan.
They are not replaced by the broader context model.

## Threat model

### Protected assets

The context compiler and broker protect:

- API keys, OAuth tokens, refresh tokens, passwords, signing material, and
  provider sessions;
- keychains, Secret Service collections, private credential files, SSH/GPG
  agents, cloud configuration, Kubernetes configuration, and local service
  sockets;
- shell startup files, loader configuration, package-manager configuration,
  proxy credentials, and certificate stores;
- files outside authorized workspace and scratch roots;
- the identity of the executable, package, container image, MCP server, tool
  schema, arguments, network audience, workspace snapshot, and operation
  revision that was approved;
- Hyper Term-owned journal, receipt, log, crash-report, renderer, and diagnostic
  channels from accidental secret disclosure;
- host availability through process, time, output, memory, disk, and nested
  interaction budgets.

### Untrusted inputs and principals

The following cannot grant themselves authority:

- model output and tool calls;
- repository files, including `.env`, MCP manifests, shell startup files,
  build scripts, package locks, and Agent instructions;
- terminal bytes, OSC sequences, child process output, and stderr diagnostics;
- local and remote MCP servers, ACP agents, provider adapters, and descendants;
- MCP tool annotations, schemas, roots, tool-list change notifications, sampling
  requests, elicitation requests, Tasks, Apps, and extensions;
- WebView, generated UI, and native presentation state;
- package-manager resolution, mutable tags, URLs, redirects, DNS results, and
  unverified executable names.

### In-scope attacks

The implementation must defend against:

- ambient host environment and credential inheritance;
- startup-code injection through shell, loader, language-runtime, and proxy
  variables;
- a workspace `.env` or MCP manifest overriding Rust-reserved control values;
- secret values appearing in JSON, debug output, logs, receipts, command
  history, process arguments, or ordinary non-keyed SHA-256 inputs;
- a mutable `npx -y package`, package tag, symlink, or PATH change substituting
  different server code after approval;
- reuse of an approval after executable, package, protocol, extension set, tool
  schema, credential binding, roots, sandbox, or arguments change;
- a broad, long-lived MCP credential being presented as call-scoped authority;
- an MCP server acting before or outside an approved tool call;
- child-process inheritance extending credential exposure beyond the claimed
  lifetime;
- local MCP roots being treated as filesystem enforcement;
- remote OAuth token passthrough, audience confusion, redirect confusion, and
  false claims of local syscall enforcement over a remote service;
- recursive sampling, elicitation, nested tools, Apps, or Tasks exhausting
  model, tool, process, output, or wall-time budgets;
- disconnect or cancellation being reported as failure when an opaque effect
  may already have executed.

### Explicit limitations

Hyper Term cannot keep a credential secret from the exact process or remote
service to which it is intentionally exposed. It can minimize, bind, supervise,
and record that exposure.

Expiry of a credential lease prevents new dispatch. It cannot revoke bytes
already copied into a running process environment. Environment exposure must
therefore be reported as process-tree or server-lifetime exposure unless the
receiver uses a broker-aware one-shot mechanism.

Hyper Term cannot enforce syscalls or internal credential handling on a remote
MCP server. It can authorize the connection and call, bind OAuth audience and
scope, limit local request and response handling, and record an opaque remote
effect.

Compromised host administrators, kernel vulnerabilities, malicious secret
providers, hardware attacks, and side channels remain outside this ADR's first
boundary.

## Decision

Hyper Term will introduce a Rust-owned execution-context compiler. Bash, Zsh,
ACP, local MCP, remote MCP, provider adapters, and supervised tools remain
separate execution adapters, but they compile the same context, authority, and
evidence primitives.

The authority flow is:

```text
user / Agent / ACP / MCP / IDE / CI intent
  -> create EffectProposal
  -> build ExecutionContextSpec
  -> resolve identities and compile ResolvedExecutionContext
  -> create immutable Operation revision
  -> evaluate policy and exact escalation
  -> authorize the exact Operation revision
  -> compile the effective sandbox or remote enforcement boundary
  -> resolve CredentialLease metadata for the authorized operation
  -> issue one-use CapabilityLease bound to operation and context digests
  -> build AuthorizedExecutionPlan
  -> commit Dispatching to the journal
  -> materialize one launch or remote request inside the daemon
  -> consume the leases
  -> execute through the selected sandbox or remote adapter
  -> supervise lifetime, budgets, and nested effects
  -> record ContextReceipt + effect receipt
```

The context is never passed to a child as a generic authority token. A child
receives only the concrete environment values, files, descriptors, sockets,
HTTP headers, mounts, and network path compiled for that dispatch.

### Authority binding

Every authorized dispatch binds at least:

```text
operation ID and revision
actor identity
effect kind
action digest
execution-context digest
sandbox-profile digest or remote-boundary identity
executable/server identity
workspace or root snapshot identity
ordinary-environment-plan digest
credential-binding identities
network audience and policy
resource budgets
issued-at, expiry, and one-use state
```

MCP calls additionally bind the negotiated protocol version, extension set,
tool contract, arguments digest, and server runtime identity.

Changing any bound value invalidates the lease and any cached exact approval.

## Context model

The exact Rust types will evolve with implementation. The following shape fixes
the authority boundaries rather than field spelling:

```rust
pub struct ExecutionContextSpec {
    pub context_id: String,
    pub context_revision: u64,
    pub mode: ExecutionMode,
    pub workspace: WorkspaceContextSpec,
    pub shell: Option<ShellContextSpec>,
    pub environment: EnvironmentPlan,
    pub credentials: Vec<CredentialRequirement>,
    pub tools: Vec<ToolRuntimeSpec>,
    pub sandbox: SandboxProfile,
    pub approval: ApprovalPolicy,
    pub resources: ResourceLimits,
    pub lifetime: ContextLifetime,
    pub evidence: EvidencePolicy,
}

pub struct ResolvedExecutionContext {
    pub context_id: String,
    pub context_revision: u64,
    pub context_digest: ContextDigest,
    pub workspace: ResolvedWorkspaceContext,
    pub shell: Option<ResolvedShellContext>,
    pub environment: ResolvedEnvironmentPlan,
    pub credential_bindings: Vec<CredentialBindingMetadata>,
    pub tool_identities: Vec<ToolRuntimeIdentity>,
    pub requested_sandbox: SandboxProfile,
    pub resources: ResourceLimits,
    pub lifetime: ContextLifetime,
    pub evidence: EvidencePolicy,
}

pub struct AuthorizedExecutionPlan {
    pub operation_id: OperationId,
    pub operation_revision: u64,
    pub context_digest: ContextDigest,
    pub action_digest: ActionDigest,
    pub enforcement: AuthorizedEnforcement,
    pub credential_leases: Vec<CredentialLeaseMetadata>,
    pub capability_lease: CapabilityLease,
}
```

`ResolvedExecutionContext` may be cloned or serialized only if all fields are
metadata. `AuthorizedExecutionPlan` is valid only for the bound operation and
lease. Raw credentials are introduced later through an opaque
`MaterializedLaunch` held by the daemon.

## Environment planning

### Bindings, not a final map

An environment plan records intent and provenance:

```rust
pub struct EnvironmentPlan {
    pub inheritance: EnvironmentInheritance,
    pub bindings: Vec<EnvironmentBindingSpec>,
    pub collision_policy: CollisionPolicy,
}

pub struct EnvironmentBindingSpec {
    pub target_name: String,
    pub source: EnvironmentSource,
    pub class: EnvironmentClass,
    pub scope: BindingScope,
    pub lifetime: BindingLifetime,
    pub override_policy: OverridePolicy,
}

pub enum EnvironmentSource {
    Literal { value: String },
    HostVariable { name: String },
    EnvFileKey {
        path: PathBuf,
        key: String,
        expected_digest: Option<Sha256Digest>,
    },
    SecretReference(SecretReference),
    Derived(DerivedBinding),
}
```

There is no user-controlled `redact: bool`. Serialization, display, hashing,
and retention behavior are derived from the source, class, and trusted policy.
A workspace cannot relabel a credential as ordinary configuration.

`Literal` values are forbidden for credential-class bindings. A secret value
must enter through a provider reference or an explicitly approved host import.

### Environment classes

At minimum, policy distinguishes:

| Class | Examples | Default Agent/MCP policy |
| --- | --- | --- |
| Runtime semantic | `PATH`, `HOME`, `TMPDIR`, `LANG`, `TZ`, `TERM` | Compile from the selected mode |
| Shell startup | `BASH_ENV`, `ENV`, `ZDOTDIR` | Deny |
| Loader or code injection | `NODE_OPTIONS`, `PYTHONPATH`, `LD_PRELOAD`, `DYLD_*` | Deny |
| Network control | `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, CA paths | Reserve for the daemon |
| Authority handle | SSH/GPG agent, Docker socket, `KUBECONFIG` | Broker as a capability |
| Credential | Tokens, keys, client secrets | Require a credential reference |
| Tool configuration | Project IDs, model names, read-only flags | Allow through normal policy |
| Observability | Trace IDs, bounded `OTEL_*` | Separate, no secret content |

Classification uses exact built-in rules and trusted policy. Name heuristics may
produce warnings or a stricter default, but cannot be the only secret boundary.

### Execution modes

Hyper Term defines three context modes:

#### `hermetic`

The default for Agents, CI tasks, and untrusted local MCP servers:

- clear the inherited host environment;
- create private `HOME` and `TMPDIR`;
- use an explicit `PATH`, locale, timezone, terminal type, and proxy;
- skip user shell startup files;
- resolve only declared credentials and authority handles.

#### `project`

Starts from the hermetic baseline and adds reviewed workspace configuration,
selected cache roots, declared `.env` keys, and project credential references.

An `.env` file is a configuration source, not a secret store. A workspace must
name the allowed file and keys, and may pin the file digest:

```yaml
envFiles:
  - path: .env.agent
    keys: [NODE_ENV, LOG_LEVEL]
    digest: optional-sha256
```

Loading every key from a workspace `.env` into every child is forbidden.

#### `user`

Preserves real user `HOME`, shell startup files, PATH, keychain helpers, and
interactive semantics for an ordinary terminal. It is marked host-dependent
and non-hermetic.

`user` mode is not available through the Agent effect port, MCP server launch,
or an inferred model request. An Agent may interact with a user terminal only
through the explicit `InputLease` boundary defined by ADR 0014.

### Layering and reserved values

The compiler applies layers in this order:

```text
1. built-in safe baseline
2. selected runtime profile
3. workspace-approved configuration
4. invocation overrides
5. authority-derived bindings
6. credential-broker bindings
```

Later does not universally mean stronger or allowed to override. Each binding
has an override policy. Security and authority values are reserved and fail
closed on collision. Initial reserved names include:

```text
HTTPS_PROXY
HTTP_PROXY
NO_PROXY
HYPER_OPERATION_ID
HYPER_CONTEXT_DIGEST
HYPER_CONTROL_FD
HYPER_CREDENTIAL_SOCKET
```

Workspace config, shell startup, and MCP manifests cannot override reserved
values.

## Shell identity and startup semantics

Shell configuration is part of the context identity. It records:

- canonical shell executable and digest where applicable;
- invocation mode: interactive, login, command, or script;
- startup-file policy;
- effective `HOME`, `ZDOTDIR`, and related runtime paths;
- PTY versus pipe transport;
- cwd and workspace identity;
- terminal encoding, locale, and `TERM` semantics;
- user-authority versus Agent-authority mode.

The compiler must not describe `zsh -lc` with a real home directory as clean or
hermetic merely because the environment map was cleared before launch.

Ordinary terminals continue to preserve login and interactive behavior, job
control, signals, process groups, resize, UTF-8/CJK input, and final output.
Agent shells use explicit startup semantics and cannot silently source user
configuration.

## Credential model

### References and providers

Serializable configuration stores references only:

```rust
pub struct SecretReference {
    pub provider_id: String,
    pub secret_id: String,
    pub version: Option<String>,
}

pub trait SecretProvider {
    fn resolve(
        &self,
        reference: &SecretReference,
        request: &CredentialRequest,
    ) -> Result<CredentialLease, SecretError>;
}
```

Initial provider candidates are explicit host-environment import, private local
files, macOS Keychain, Linux Secret Service, and an OAuth token store. External
Vault or password-manager integrations are providers, not reasons for Hyper
Term to become a vault.

Workspace configuration may request a credential requirement but cannot choose
an arbitrary user secret. Trusted user or managed policy maps requirements to
allowed references.

### Exposure methods

Credential delivery is explicit:

```rust
pub enum ExposureMethod {
    Environment { name: String },
    HttpHeader { name: String },
    PrivateFile { target: PathBuf },
    InheritedFileDescriptor,
    UnixSocketBroker,
    HostMediated,
}
```

Preferred delivery, when the receiver supports it, is:

```text
host-mediated typed operation
  -> short-lived audience-bound token
  -> broker socket or inherited descriptor
  -> private temporary file
  -> environment variable
  -> command-line argument
```

Legacy compatibility may require environment delivery. The receipt must report
the actual exposure method and effective process-tree lifetime rather than a
stronger intended lifetime.

Command-line secret delivery is denied by default because arguments commonly
enter process listings, logs, histories, crash reports, and diagnostics.

### Digests and persistence

Ordinary environment values remain canonical digest inputs. Credential values
do not.

A credential binding digest contains only stable metadata where available:

```text
provider ID
secret ID
provider version
credential lease ID
audience and scopes
target identity
exposure method
issued-at and expiry
```

A provider that cannot expose a stable secret version produces a unique lease
identity and prevents approval reuse across resolutions. Hyper Term does not
place the raw secret into a normal SHA-256 digest to simulate identity.

The daemon redacts before journal, log, stderr, crash, renderer, and receipt
boundaries. Redaction is defense in depth; the primary invariant is that raw
values never enter those data structures.

## Local MCP authority

### Server launch is an effect

Starting a local stdio MCP server is separately authorized from invoking one of
its tools. The launch operation binds:

- installation and executable identity;
- arguments and cwd;
- environment and credential binding metadata;
- filesystem, network, process, and resource policy;
- roots or workspace snapshot identity;
- lifecycle and restart policy.

The server may execute code immediately after launch, before discovery or a
tool call. Tool approval is therefore never a substitute for server-launch
containment.

### Installation identity

The first implementation accepts identities Hyper Term can actually prove:

- an absolute canonical executable plus SHA-256;
- a bundled package with a frozen lockfile and bounded file inventory;
- a reviewed package installation with package name, version, declared bin,
  runtime identity, and content inventory;
- a container image digest.

An unversioned or mutable `npx -y package`, package tag, PATH-only executable,
or same-named replacement is rejected for automatic Agent execution. Hyper Term
does not attempt to infer a trustworthy transitive package identity from an
arbitrary package-manager command line in the first implementation.

### Runtime and tool identity

A server runtime identity binds:

```text
installation identity
negotiated protocol version
negotiated extension set
resolved execution-context digest
credential binding set
workspace/root snapshot
sandbox and network policy
lifecycle
```

A tool contract identity binds:

```text
server runtime identity
tool name
canonical input-schema digest
canonical output-schema digest when present
host-owned effect classification
required credential capabilities
timeout and output budget
tool catalog revision
```

Server-provided read-only or destructive annotations are untrusted UI hints.
Host policy determines the effective effect class.

Changes to installation identity, runtime identity, protocol, extensions, tool
catalog, schema, credentials, roots, sandbox, or network policy invalidate old
tool approvals and cached discovery.

### Isolation levels

Hyper Term reports one of three local MCP credential-isolation levels.

#### Level 1: server-lifetime legacy

One server receives one fixed environment, credential set, and sandbox for its
lifetime. Per-call approval controls Hyper Term's dispatch but cannot claim to
restrict what the already-credentialed server does internally.

Receipts state:

```text
credential_scope: server_lifetime
per_call_isolation: false
```

#### Level 2: split by privilege

Separate server instances use distinct minimal credentials, tool allowlists,
network rules, and approval policies. For example, a read-only GitHub server and
an issue-writing server do not share a token or lifecycle.

This is the first practical least-privilege target for compatible existing MCP
servers. Tool allowlists do not replace credential scope or process isolation.

#### Level 3: broker-aware per-call worker

A low-authority control process requests a one-use credential for an exact tool
contract, arguments digest, operation revision, audience, and short expiry. A
one-shot worker or host-mediated operation consumes it.

Only a server or worker protocol that cooperates with this design may be
reported as call-isolated. Hyper Term never infers Level 3 from per-call UI
approval alone.

### Roots

MCP roots are context and UX inputs. Hyper Term compiles reviewed roots into
filesystem policy or private snapshots, but does not treat the MCP roots list as
an OS security boundary.

## Remote MCP authority

A remote HTTP MCP service does not receive a local process sandbox. Hyper Term
controls:

- whether to connect and which canonical endpoint identity is allowed;
- which user or service identity is used;
- OAuth resource, audience, scopes, and refresh policy;
- which server and tool contract is invoked with which arguments;
- request, response, timeout, retry, output, and nested-interaction budgets;
- durable operation and receipt state.

The remote service controls its own files, network, processes, data retention,
and downstream calls. Receipts distinguish:

```rust
pub enum EnforcementBoundary {
    LocalProcess { backend: SandboxBackendKind },
    RemoteService {
        endpoint_identity: String,
        oauth_resource: Option<String>,
        auth_audience: Option<String>,
    },
}
```

Tokens accepted by an MCP server are audience-bound to that server. A remote
MCP server that calls another API must obtain a separate downstream credential;
Hyper Term does not configure or endorse token passthrough.

Remote connection establishment and each opaque effect preserve
`UnknownExecution` semantics. A disconnected request is not silently replayed
when the remote effect may have executed.

## Nested MCP interactions and protocol evolution

MCP may introduce nested user input, model sampling, tools, Apps, Tasks, and
extensions. The stable `2025-11-25` protocol and the locked `2026-07-28` release
candidate organize some of these capabilities differently. Hyper Term therefore
does not make a current MCP callback taxonomy its durable authority model.

Every nested behavior maps to an operation or bounded interaction carrying:

```text
causation ID
correlation ID
parent operation ID
recursion depth
remaining tool budget
remaining model budget
remaining process and output budget
remaining wall time
credential scope
negotiated protocol and extension identity
```

The existing event envelope's causation and correlation fields are reused and
populated rather than replaced with an MCP-specific evidence graph.

Form-mode elicitation cannot collect passwords, API keys, access tokens, or
other credentials. Credential acquisition uses a trusted provider or an
explicit out-of-band URL/OAuth flow whose destination and server identity are
shown to the user. The model, conversation, and ordinary MCP payload do not see
the resulting secret.

## Northbound and southbound MCP

The existing `hyper-term-mcp` remains a bounded northbound server through which
an Agent can propose built-in Hyper Term effects. It does not become the core
authority protocol; its requests still map to `EffectProposal`, `Operation`,
`CapabilityLease`, and receipts.

A future southbound MCP client may connect to reviewed local or remote servers
through the same broker. A future public northbound surface may expose bounded
operations such as context inspection, workspace diff, or receipt lookup.

A generic `shell.run` or arbitrary `mcp.call` northbound API is deferred until
it has:

- authenticated caller identity;
- recursion and re-entry protection;
- exact context and operation binding;
- a stable public protocol and compatibility policy;
- proof that it cannot bypass the normal Terminal/Agent separation.

## Configuration example

The eventual configuration may resemble:

```yaml
contexts:
  repo-dev:
    mode: hermetic
    workspace:
      root: .
      metadata:
        git: read
        agentInstructions: read

    shell:
      executable: /bin/zsh
      invocation: command
      startup: clean

    environment:
      import: [LANG, LC_ALL]
      set:
        CI: "1"
        NODE_ENV: test
      deny:
        - BASH_ENV
        - ENV
        - ZDOTDIR
        - NODE_OPTIONS
        - PYTHONPATH
        - "DYLD_*"
        - LD_PRELOAD

    credentials:
      github-read:
        requirement: github-repository-read

    mcp:
      github-read:
        transport: stdio
        executable: /absolute/path/to/github-mcp
        executableSha256: "..."
        credentials: [github-read]
        roots: [workspace]
        sandbox:
          filesystem: read-only
          network:
            allowedDomains: [api.github.com]
        tools:
          allow: [get_file_contents, list_issues, get_issue]
        lifecycle: one-task
```

The committed file contains requirements and references, not tokens, refresh
tokens, secret values, or pre-authenticated URLs.

## Component boundaries

This decision fits the current workspace rather than creating a parallel
`hyper-exec-*` stack immediately.

### `hyper-term-protocol`

Owns serializable context specifications, environment binding metadata,
credential requirements and references, context digests, enforcement-boundary
DTOs, capability-lease bindings, and redacted receipts.

It never owns raw credential values or provider clients.

### `hyper-term-core`

Owns platform-neutral context validation, deterministic layering, collision
rules, authority binding, digest inputs, policy reduction, operation state, and
ports for identity, secret, sandbox, and remote-service resolution.

It remains independent of Tauri, Native SDK, WebView, keychain APIs, MCP SDK
implementation details, and platform sandbox syntax.

### `hyper-term-sandbox`

Consumes the ordinary environment projection, filesystem/network/process
policy, and opaque materialization handles needed by a platform launcher. It
enforces local process-tree boundaries and reports the actual backend.

Raw secret delivery is coordinated with the daemon at the final spawn boundary
and never serialized into a compiled SBPL, command preview, or receipt.

### `hyper-term-drivers`

Owns protocol and process adapters. A driver declares context requirements and
receives a brokered launch; it does not resolve host environment, secrets, or
approval on its own.

MCP SDK and provider-specific wire churn remain here rather than entering the
durable operation model.

### `hyper-term-daemon`

Owns trusted configuration sources, provider implementations, credential
resolution, opaque materialization, lease consumption, dispatch, process and
remote-call supervision, journal writes, redaction, and receipts.

### Native and WebView surfaces

Presentation may show a redacted context plan, credential requirement, exact
escalation, server/tool identity, lifetime, and receipt. It cannot read secret
values, construct sandbox policy, select a secret reference on behalf of a
workspace, or launch a process.

## Inspection and future CLI

The daemon will eventually expose read-only, redacted operations equivalent to:

```text
context validate <context>
context plan <context> --target shell
context plan <context> --target mcp:<server>
context diff <left> <right>
context explain <operation-id>
mcp inspect <server>
receipt show <operation-id>
```

The first interface may be a `hyperd` subcommand or a thin client of the
existing control protocol. It does not require a second daemon or a new product
brand.

## Extraction criteria

The execution-context boundary is intentionally reusable, but Hyper Term will
not create a separate `hyper-exec` repository or family of crates solely from
the architectural possibility.

Extraction is reconsidered when all of the following are true:

1. a production consumer outside the desktop application uses the same context
   compiler and broker, such as an IDE host, CI runner, or independently shipped
   headless service;
2. the control protocol is versioned and contains no desktop or renderer types;
3. context, credential-provider, and receipt contracts have survived at least
   two adapters without provider-specific fields entering the core;
4. independent release, security maintenance, or dependency ownership is
   materially simpler than the existing workspace;
5. integration tests prove that extraction preserves operation, sandbox,
   terminal, and failure semantics.

Until then, Hyper Term creates a cleanly extractable boundary inside the current
five-crate workspace.

## Implementation plan

### Phase 1: context metadata and ordinary environment compilation

- add `ExecutionContextSpec`, `EnvironmentPlan`, binding classes, provenance,
  collision rules, context digest, and redacted `ContextReceipt` DTOs;
- keep credential sources reference-only and introduce no production secret
  provider yet;
- compile hermetic and project environment plans into the existing sandbox and
  driver launch paths;
- migrate one ACP/provider launch and the bundled `hyper-term-mcp` launch to the
  context compiler;
- split ordinary action/environment digests from credential-binding metadata;
- populate existing causation and correlation fields for the migrated path.

### Phase 2: generic local stdio MCP at Levels 1 and 2

- adopt the reviewed official Rust MCP client boundary for a minimal stdio
  feature set;
- authorize server launch separately from tool calls;
- require pinned installation identity and reject mutable automatic launch;
- record negotiated protocol, extension set, canonical tool schemas, catalog
  revision, roots snapshot, sandbox, credentials, and lifecycle;
- invalidate cached approvals and discovery on identity change;
- implement server-lifetime receipts and split-by-privilege instances;
- make no per-call secret-isolation claim.

### Phase 3: credential providers and materialized launch

- implement explicit host import and private-file providers first;
- add macOS Keychain and Linux Secret Service through daemon-owned providers;
- introduce opaque `CredentialLease` and `MaterializedLaunch` types;
- deliver environment and private-file credentials at the final launch boundary;
- prove raw values cannot reach protocol JSON, logs, debug output, journal,
  renderers, crash diagnostics, or ordinary digests;
- terminate the complete process tree when a server credential lease or context
  lifetime ends.

### Phase 4: remote HTTP MCP and OAuth

- add canonical endpoint identity and reviewed Streamable HTTP support;
- implement OAuth resource indicators, audience validation metadata, scope
  escalation, refresh, and explicit out-of-band credential acquisition;
- forbid token passthrough configurations;
- distinguish remote enforcement receipts from local sandbox receipts;
- preserve `UnknownExecution` and no-silent-replay behavior across disconnects.

### Phase 5: nested interactions and cooperative per-call credentials

- add recursion, model, tool, process, output, and wall-time budgets;
- map elicitation, sampling/input requests, Apps, Tasks, and extensions into
  correlated operations or bounded interactions;
- provide an optional broker socket or one-shot worker SDK;
- report Level 3 only for integrations that consume an exact one-use credential
  lease at the call boundary.

### Phase 6: prove external reuse before extraction

- expose redacted validation, plan, diff, explain, and receipt APIs;
- integrate one non-desktop consumer against the versioned daemon boundary;
- measure whether a separate crate, binary, repository, and release lifecycle
  reduce coupling in practice;
- extract only the proven component set.

## Validation gates

Every protocol or lifecycle change receives tests in the owning crate.

### Context compiler

- canonical ordering produces byte-stable context and environment-plan digests;
- inheritance, collision, override, wildcard deny, and reserved-name rules are
  deterministic and fail closed;
- workspace configuration can narrow but cannot widen managed or user policy;
- changing executable, cwd, ordinary environment, roots, sandbox, credentials,
  budgets, protocol, extensions, schema, or arguments changes the expected
  binding;
- invalid paths, NULs, non-UTF-8 boundaries, duplicate bindings, unknown classes,
  oversized files, and incomplete `.env` parsing fail safely;
- hermetic and project plans cannot load user startup files through derived
  values or indirect variables.

### Secrets and redaction

- secret-containing fixtures cannot be found in serialized protocol messages,
  journal files, receipts, command previews, logs, debug formatting, Hyper
  Term-owned crash diagnostics, or renderer snapshots;
- credential-class literals and command-line exposure are rejected by default;
- provider resolution occurs only after exact operation authorization;
- stale, expired, wrong-audience, wrong-scope, wrong-target, reused, and
  operation-mismatched credential leases are rejected;
- provider failure leaves no partial file, environment mutation, child process,
  or falsely successful receipt;
- private files and sockets have restrictive ownership and are destroyed on all
  success, failure, cancellation, crash-recovery, and timeout paths;
- process-tree lifetime is reported honestly for environment exposure.

### Local MCP

- server code cannot start before its launch operation reaches `Dispatching`;
- executable, package, lockfile, inventory, or symlink substitution fails before
  launch;
- tool-list and schema changes invalidate cached exact approvals;
- server annotations never lower host policy;
- roots do not grant filesystem access beyond the compiled sandbox;
- a Level 1 server is never labelled per-call isolated;
- split read/write server fixtures receive distinct credentials and policies;
- server activity before a tool call remains inside the authorized launch
  sandbox and receipt lifetime;
- kill or disconnect before, during, and after a call produces the correct
  `Failed` or `UnknownExecution` result without replay.

### Remote MCP

- endpoint, resource, audience, scope, redirect, and protocol mismatches fail
  closed;
- inbound MCP tokens are never reused as downstream API credentials;
- retries require an idempotency contract or a new explicit operation;
- remote receipts never claim a local sandbox backend;
- URL-mode credential acquisition displays the exact requesting server and
  destination and does not expose entered credentials to the model or client
  transcript.

### Nested interactions

- causation and correlation form an acyclic bounded tree;
- recursion depth and every resource budget are enforced across adapters;
- cancellation propagates to descendants while preserving uncertain effect
  states;
- nested requests cannot reuse parent credentials outside their audience,
  scope, lifetime, or operation binding;
- malformed or unsupported extensions remain bounded diagnostic data and cannot
  mutate canonical operation state.

### Terminal and sandbox regression

- ordinary `New` remains a normal user terminal and preserves Zsh/Bash startup,
  PTY, resize, job control, signals, UTF-8/CJK input, and final output;
- Agent and MCP execution cannot select `user` mode or acquire a normal terminal
  without an `InputLease`;
- macOS Seatbelt and isolated-task backends receive equivalent ordinary context
  semantics without exposing raw credential values in compiled policy text;
- unsupported enforcement fails closed;
- Rust gates remain `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace`.

Workbench or desktop presentation changes additionally run `deno task check`,
`deno task test`, and `deno task build:workbench`.

## Consequences

### Positive

- Shell, ACP, MCP, supervised tools, and future hosts gain one explainable
  context and evidence model without making MCP the authority protocol.
- Environment behavior becomes explicit and reviewable instead of a flat map.
- Secret references and raw secret material have separate type and process
  boundaries.
- Local MCP receipts honestly distinguish server-lifetime, split-privilege, and
  cooperative per-call isolation.
- Approval reuse becomes safe across executable, schema, credential, root,
  sandbox, and protocol changes.
- The design can later power a CLI, IDE, or CI consumer without moving machine
  authority into the renderer.

### Costs

- Context compilation adds types, digests, policy errors, migration work, and
  user-visible explanations before generic MCP support ships.
- Secret providers require platform-specific implementations, lifecycle tests,
  and careful crash cleanup.
- Many third-party MCP servers remain Level 1 because their credential model is
  inherently server-lifetime.
- Exact installation identity intentionally rejects convenient mutable launch
  forms until they are pinned or reviewed.
- Remote MCP remains an opaque external trust boundary even with correct OAuth
  and receipts.

### Compatibility

Existing built-in drivers migrate incrementally. A compatibility adapter may
convert a reviewed flat ordinary environment map into bindings marked with
explicit provenance and process lifetime. It cannot infer credential safety or
upgrade a secret-bearing legacy map to a stronger isolation claim.

Old serialized profiles remain readable only through a versioned migration that
marks unknown provenance and requires reauthorization. They are not silently
treated as fully resolved contexts.

## Rejected alternatives

### Keep environment as a flat map

Rejected because final values cannot express provenance, sensitivity, scope,
lifetime, exposure, collision authority, or safe receipt behavior.

### Make `RuntimeContext` the bearer unit of authority

Rejected because a reusable context would recreate ambient authority. Contexts
are compilation inputs; immutable operation revisions and one-use leases
authorize dispatch.

### Treat per-tool approval as per-call credential isolation

Rejected because a local server may already possess and use a server-lifetime
credential before and between approved tool calls.

### Hash raw secrets into the ordinary action digest

Rejected because it mixes secret bytes with serializable command identity,
creates additional disclosure and offline-guessing surfaces, and still does not
express provider, audience, scope, or lifetime.

### Load complete workspace `.env` files

Rejected because workspace files are untrusted, frequently contain unrelated
credentials, and should not grant every descendant the union of all project
authority.

### Trust MCP roots, annotations, or tool names as enforcement

Rejected because they are server-provided context and hints, not local OS
policy or verified effect identity.

### Resolve arbitrary mutable package-manager commands

Rejected for the first implementation because a command such as unversioned
`npx -y` does not provide a stable package, runtime, executable, or content
identity. Exact reviewed forms can be added without weakening the default.

### Build a parallel `hyper-exec-*` crate and daemon family now

Rejected until a real external consumer and stable protocol prove independent
ownership. The existing five-crate workspace already has the required authority
and portability seams.

### Use MCP as the durable authority protocol

Rejected because MCP versions and extensions evolve independently, other effect
adapters exist, and external protocol messages cannot become canonical machine
authority or journal state.

## References

- [MCP 2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [MCP 2025-11-25 tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
- [MCP 2025-11-25 elicitation](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation)
- [MCP authorization and local stdio credentials](https://modelcontextprotocol.io/docs/tutorials/security/authorization)
- [MCP security best practices](https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices)
- [MCP 2026-07-28 release candidate](https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/)
- [VS Code MCP configuration and local sandbox](https://code.visualstudio.com/docs/agents/reference/mcp-configuration)
- [GNU Bash startup files and `BASH_ENV`](https://www.gnu.org/software/bash/manual/bash.html)
