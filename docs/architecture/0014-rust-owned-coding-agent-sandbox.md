# ADR 0014: Enforce Coding Agent execution through layered Rust-owned sandboxes

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md),
  [ADR 0008](0008-license-scoped-warp-reuse.md), and
  [ADR 0009](0009-rust-acp-mcp-adapters.md)
- Refines: the local-process containment requirements in
  [ADR 0003](0003-brokered-deno-sidecar.md) and
  [ADR 0009](0009-rust-acp-mcp-adapters.md)

## Context

Hyper Term is both a normal terminal and a host for Coding Agents. These are
different authority models:

- a normal terminal is an explicit user-controlled shell with the user's normal
  machine authority;
- an Agent terminal accepts commands, tool requests, generated code, repository
  content, and terminal output that may all be untrusted;
- a local ACP agent, MCP server, CLI agent, compiler, test runner, or generated
  executable may create child processes and perform effects that cannot be
  reconstructed or intercepted by parsing terminal output.

The Rust kernel already owns PTYs, process groups, operation revisions,
permission decisions, executable digests, sanitized driver environments, and
the operation journal. An authorized shell operation currently reaches
`TerminalSupervisor::spawn`, which creates a `portable_pty::CommandBuilder` for
the requested program, arguments, cwd, and environment. Driver processes also
canonicalize their executable and cwd, verify the executable SHA-256, clear the
ambient environment, and create a process group.

Those controls establish identity, lifecycle, and auditability, but they are not
an operating-system security boundary. After spawn, an unsandboxed process can
still read user secrets, modify files outside the workspace, connect to the
network, access local sockets such as the Docker daemon, signal other
processes, or delegate the same effect to a child.

Approval is also not containment. A user may approve `cargo test` for one
workspace without intending to grant every executable in the resulting process
tree access to the home directory, credentials, local services, or unrestricted
network. Conversely, a strong sandbox can allow routine work to proceed without
prompting for every harmless command.

Hyper Term therefore needs a sandbox boundary that is compiled and enforced by
Rust at the final execution boundary. It must preserve normal interactive PTY
semantics while preventing the model, terminal output, WebView, Deno sidecar,
agent process, and MCP server from granting themselves authority.

## Decision drivers

The design is driven by the following requirements:

1. Rust remains the only machine-authority and permission decision point.
2. `hyper-term-core` stays independent from Tauri, Native SDK, React, and any
   platform-specific sandbox API.
3. Ordinary user terminals remain ordinary shells. Agent execution is an
   explicit product mode and cannot silently inherit a user terminal's ambient
   authority.
4. Every child process inherits an enforcement boundary. Command-name analysis
   and model-provided risk labels are hints, never the security boundary.
5. Filesystem, network, environment, secret, process, and resource access are
   separate capabilities with least-privilege defaults.
6. Approval and sandbox policy remain separate: approval may authorize a
   bounded escalation, while the sandbox enforces the resulting boundary.
7. A platform that cannot enforce the compiled profile must fail closed rather
   than silently launch without containment.
8. PTY behavior must preserve interactive shells, job control, resize, signals,
   UTF-8, CJK input, and final output after process exit.
9. Local fast paths and hermetic task environments share one policy model but
   may use different enforcement backends.
10. All policy compilation, escalation, dispatch, violation, cancellation, and
    acceptance decisions are durable, correlated operation events.

## Threat model

### Protected assets

The sandbox protects:

- files outside the authorized workspace or scratch roots;
- repository control and instruction metadata, including `.git`, `.hyper-term`,
  `.agents`, `.codex`, `.claude`, `AGENTS.md`, and `CLAUDE.md` unless explicitly
  granted;
- environment files, SSH/GPG keys, cloud credentials, browser data, keychains,
  provider tokens, and other user secrets;
- the host network, local/private/link-local services, Unix-domain sockets, and
  privileged endpoints such as Docker or container-runtime sockets;
- unrelated host processes, sessions, clipboard, accessibility, camera,
  microphone, and GUI automation capabilities;
- the integrity of the approved command, cwd, executable, policy revision,
  worktree, accepted artifacts, and operation journal;
- host availability through process, CPU, memory, file, and output bounds.

### Untrusted principals and inputs

The following are untrusted by default:

- model text, model tool calls, and model-supplied risk or read-only labels;
- repository source, build scripts, dependencies, tests, hooks, generated code,
  downloaded artifacts, and executable output;
- terminal bytes and escape sequences, including forged OSC markers;
- local or remote ACP agents, MCP servers, CLI agents, and their descendants;
- generated UI and WebView messages;
- path strings, symlinks, globs, environment values, URLs, DNS answers, and
  redirect targets supplied by an operation.

### In-scope attacks

The implementation must defend against:

- reads or writes outside approved roots, including `..`, symlink, hard-link,
  rename, mount, and time-of-check/time-of-use variants;
- mutation of repository metadata or Agent instruction files through a broader
  writable parent;
- secret access through files, environment inheritance, process inspection,
  keychain helpers, local sockets, or package-manager configuration;
- network exfiltration, direct-socket bypass of an allowlist proxy, DNS
  rebinding, redirects, and access to loopback or private networks;
- escape through child processes, shell wrappers, interpreters, compilers,
  debuggers, `ptrace`, cross-process signals, or setuid/capability transitions;
- approval substitution, including command/cwd/profile changes after approval;
- output, process-count, CPU, memory, disk, or wall-time exhaustion;
- reuse of an expired or already-consumed capability grant;
- silent retry after a disconnect when the original effect may have executed;
- an Agent writing into a normal user terminal without an explicit `InputLease`.

Kernel vulnerabilities, malicious host administrators, physical attacks, and
hardware side channels are outside the initial boundary. A remote tool remains
an opaque external effect; Hyper Term can authorize and record it but cannot
claim local syscall enforcement over the remote host.

## Decision

Hyper Term will introduce a Rust-owned, layered Coding Agent sandbox. It has one
declarative policy model and two enforcement tiers.

### Tier 1: native local command sandbox

Tier 1 is the default for interactive Agent commands, builds, tests, and tools
that need low-latency access to an authorized local workspace.

- macOS uses a generated Seatbelt profile and a supervised wrapper launch;
- Linux uses bubblewrap for the filesystem and namespace view, plus seccomp and
  `PR_SET_NO_NEW_PRIVS` for syscall and privilege restrictions;
- native Windows uses a restricted-token backend, with a stronger isolated-user
  or elevated backend when available;
- unsupported platforms or profiles return an explicit unsupported-policy
  result. They do not degrade to an unsandboxed spawn;
- a compatibility backend may implement a strictly narrower profile, but the
  policy compiler must prove that it is not widening the requested capability.

Tier 1 is an enforcement boundary, not a reproducible environment. It may read
host toolchains and libraries allowed by the compiled platform profile.

### Tier 2: isolated task environment

Tier 2 is used when an operation needs a more hermetic environment or contains
opaque local execution that Hyper Term cannot break into separately authorized
effects.

- create a temporary worktree or immutable source snapshot;
- launch an ephemeral container or VM with an explicit image/toolchain digest;
- do not mount the user's home directory;
- mount only the task worktree, bounded caches, private scratch space, and
  explicitly approved read-only inputs;
- route network through the same Rust-owned network policy or keep it disabled;
- inject secrets through action-scoped brokers rather than persistent files or
  ambient environment variables;
- enforce CPU, memory, process, disk, output, and wall-time limits;
- destroy the environment when the task is complete or cancelled;
- return only reviewable diffs, test evidence, and content-addressed artifacts.

Tier 2 is the default for opaque third-party agents that execute their own
commands, untrusted dependency installation, broad repository migrations, and
workloads requiring arbitrary commands without host access.

### Ordinary terminals are not Agent sandboxes

Opening a normal terminal continues to create a normal interactive user shell.
It is labeled as user-authority and is not advertised to an Agent as a generic
execution capability.

An Agent may request a terminal operation in an Agent-owned sandbox session. It
may write to an existing terminal only after the broker grants an explicit,
exclusive, bounded `InputLease` for that terminal and operation revision. A
normal terminal never becomes an implicit sandbox escape hatch.

## Authority and execution flow

All effects follow this flow:

```text
model / ACP / MCP / generated UI / user intent
  -> normalize the proposed effect and requested resources
  -> create an immutable Operation revision
  -> evaluate organization, user, workspace, and built-in policy
  -> select a base SandboxProfile
  -> compute any bounded AdditionalPermissions
  -> request exact human approval when policy requires escalation
  -> compile paths, network, environment, resources, and backend constraints
  -> produce CompiledSandboxProfile + one-use CapabilityLease
  -> commit Dispatching authority to the journal
  -> SandboxLauncher creates the process and PTY inside the boundary
  -> supervise the complete process tree and bounded output channels
  -> capture exit, violations, diff, artifacts, and verification evidence
  -> record a SandboxReceipt and terminal operation state
```

The model may request capabilities but cannot choose the effective profile,
declare a command safe, or lower the enforcement tier. Policy may choose a
stricter profile than requested.

The authority commit happens before process creation. A failure before the
process exists becomes `Failed`. Loss of supervision after a process or opaque
effect may have started becomes `UnknownExecution`; it is never silently
replayed.

## Component boundaries

The implementation is split by authority and portability:

- `hyper-term-protocol` owns serializable sandbox profile, approval, receipt,
  violation, and capability lease DTOs;
- `hyper-term-core` owns platform-neutral policy evaluation, profile reduction,
  operation state transitions, ports, and deterministic digest inputs;
- a new `hyper-term-sandbox` crate owns profile compilation and platform
  backends without depending on Tauri or a renderer;
- `hyper-term-daemon` owns the permission broker, lease issuance, dispatch,
  supervision, journal writes, and network proxy lifecycle;
- `hyper-term-drivers` must request sandboxed process creation through a core
  port. A driver cannot call `Command::spawn` for an Agent or tool workload
  outside a narrowly documented bootstrap path;
- the WebView and Native SDK shell only display profiles, proposed escalations,
  violations, diffs, and receipts. They receive no sandbox-construction or
  process-launch capability.

The normal user-shell path remains explicit and separate from the Agent effect
port. This prevents a generic `TerminalCommand` from accidentally acquiring a
privileged default.

## Policy model

The canonical protocol is declarative. Platform-specific rules such as SBPL,
bubblewrap arguments, seccomp filters, Windows ACLs, tokens, or firewall rules
are compiled artifacts and never supplied by a model or renderer.

An illustrative shape is:

```rust
struct SandboxProfile {
    profile_id: String,
    profile_revision: u64,
    enforcement: SandboxEnforcement,
    filesystem: FileSystemPolicy,
    network: NetworkPolicy,
    environment: EnvironmentPolicy,
    process: ProcessPolicy,
    resources: ResourceLimits,
    workspace: WorkspacePolicy,
    lifetime: SandboxLifetime,
}

enum PathAccess {
    Read,
    Write,
    Deny,
}

struct AdditionalPermissions {
    filesystem: Vec<PathRule>,
    network: Vec<NetworkGrant>,
    secrets: Vec<SecretGrant>,
    scope: GrantScope,
}

struct CompiledSandboxProfile {
    canonical_profile: SandboxProfile,
    backend: SandboxBackend,
    backend_version: String,
    resolved_paths: Vec<ResolvedPathRule>,
    compiled_policy_digest: Sha256Digest,
}

struct CapabilityLease {
    operation_id: OperationId,
    operation_revision: u64,
    command_digest: Sha256Digest,
    executable_digest: Option<Sha256Digest>,
    compiled_policy_digest: Sha256Digest,
    issued_to: ActorId,
    expires_at: Timestamp,
    remaining_uses: u32,
}
```

The concrete schema may differ, but it must preserve these bindings. The lease
is normally one-use. A turn- or session-scoped grant is a policy input from
which one-use leases are minted, not a reusable bearer token passed to child
processes.

### Built-in profiles

Hyper Term initially provides these profiles:

| Profile | Filesystem | Network | Intended use |
| --- | --- | --- | --- |
| `agent-read-only` | Read approved workspace/runtime roots; write private scratch; deny secrets | Off | Inspection, planning, search, static analysis |
| `agent-workspace` | Write workspace and scratch with protected carve-outs | Off | Normal editing, local checks, tests with cached dependencies |
| `agent-networked-build` | `agent-workspace` plus approved cache roots | Proxy-only, domain constrained | Explicit dependency or API access |
| `agent-isolated-run` | Ephemeral worktree/container mounts only | Off or proxy-only | Opaque agents, migrations, untrusted builds |
| `danger-full-access` | Host access | Host access | Explicit exceptional human-authorized use only |

`danger-full-access` is not selected from a model request, workspace file, Agent
profile, or inferred risk label. It requires a conspicuous exact approval and
remains auditable. Managed policy may disable it entirely.

### Policy composition

The effective policy is produced from:

```text
built-in platform maximum
  intersect organization requirements
  intersect user settings
  intersect workspace policy
  intersect operation base profile
  union only explicitly approved AdditionalPermissions
```

An additional permission cannot exceed an organization or platform maximum.
For conflicting equally specific filesystem rules, `Deny` wins over `Write`,
and `Write` wins over `Read`. More specific protected carve-outs remain in force
inside a broader writable root unless the exact protected target is explicitly
approved.

Policy files inside the workspace are untrusted inputs. They may request a
narrower profile but cannot grant more authority than user or managed policy.

## Filesystem enforcement

The filesystem is deny-by-default at the policy level.

- runtime roots required for the selected toolchain are read-only;
- workspaces and private scratch roots are granted independently;
- caches are separate roots with explicit ownership and retention;
- `.git`, `.hyper-term`, `.agents`, `.codex`, `.claude`, `AGENTS.md`, and
  `CLAUDE.md` are read-only under writable workspaces by default;
- secret patterns such as `.env`, credential files, SSH/GPG material, and cloud
  configuration are deny-read by default unless an exact operation needs them;
- Unix-domain sockets and device nodes are capabilities, not ordinary files;
  Docker, container runtime, SSH agent, GPG agent, and desktop automation sockets
  are denied unless specifically brokered;
- package installation is a distinct operation. The resulting lockfile, cache,
  and provenance digest must be accepted before a later offline operation uses
  them.

All existing roots are resolved to canonical absolute paths before profile
compilation. Glob expansion is bounded and fails closed on malformed patterns,
overflow, or incomplete scans. The platform backend must preserve protected
carve-outs after writable mounts are applied.

Canonicalization alone does not solve symlink races. Backends use mount or
kernel policy boundaries where possible, re-check identities at dispatch, avoid
following attacker-controlled links for privileged host operations, and include
symlink/rename races in adversarial tests.

## Network enforcement

Network access is off by default. A network-enabled profile uses a Rust-owned
proxy or equivalent capability gateway.

- policy identifies allowed schemes, hostnames, ports, and optionally methods;
- direct outbound sockets are blocked when proxy-only mode is selected;
- loopback, RFC1918/private, link-local, metadata-service, multicast, and local
  Unix sockets are denied by default;
- DNS results and redirect destinations are checked against the same policy;
- domain approval is not treated as permanent approval of every resolved IP;
- proxy credentials and provider tokens are injected only for the approved
  operation and are not exposed in transcripts;
- network logs retain destination metadata, decision, timing, and byte counts by
  default, not request or response bodies;
- setup/install phases may have a different profile from execution phases.

On a platform that cannot prevent direct network bypass, a proxy allowlist is
not considered strong enforcement. The command must move to Tier 2, use an
offline profile, or be rejected.

## Environment and secret handling

Agent and tool processes receive a cleared environment plus an allowlist. The
policy compiler categorizes variables as public configuration, path/toolchain,
locale/terminal, proxy routing, or secret.

Secrets are represented by broker-owned identifiers. An operation receives the
minimum credential through a pipe, inherited descriptor, short-lived proxy, or
backend-specific secret mount. The journal stores the identifier, scope, and
digest where appropriate, never the secret value. Secrets expire with the
operation and are not copied into generated artifacts, child-visible global
configuration, or persistent shell history.

`HOME`, tool-specific homes such as `CODEX_HOME`, package caches, and temporary
directories point to sandbox-private or explicitly mounted roots. They do not
default to the user's real home directory.

## PTY and process lifecycle

The PTY is created as part of the sandboxed launch, not before an unsandboxed
child is allowed to initialize arbitrary state.

- the complete descendant tree inherits the filesystem, network, and process
  restrictions;
- interactive programs retain a controlling terminal, resize, job control,
  signals, UTF-8, color, and final-output drain behavior;
- signals to unrelated host processes are denied; cancellation targets the
  sandbox process group or container/VM workload;
- process count, open files, CPU, memory, output, disk, and wall time are bounded
  by profile;
- stdout, stderr, and PTY output stay ordered and bounded and are always treated
  as untrusted data;
- an OSC sequence may improve presentation but cannot modify policy, satisfy an
  approval, mint a lease, or declare completion;
- a violation is a structured event linked to the operation, backend, rule, and
  profile digest without leaking protected content.

## Worktree, diff, and artifact acceptance

Tier 2 isolates execution from acceptance. Completing a command does not merge
its effects into the user's working tree.

- Hyper Term records the source revision and worktree/snapshot identity;
- output is reduced to a bounded diff, declared artifact set, test evidence, and
  receipt;
- the user or policy accepts exact artifact digests or an exact diff revision;
- acceptance is a new brokered operation with its own lease;
- unexpected files, special files, sockets, ownership, permissions, symlinks,
  and paths outside the declared result set block automatic acceptance;
- cancellation or environment destruction does not imply that a remote effect
  was rolled back.

The implemented Artifact acceptance slice applies a bounded set of one to 32
UTF-8 files from the current immutable GenUI Artifact to explicit, unique
workspace-relative paths. It is one digest-bound `FileEdit / WorkspaceWrite`
operation rather than a side effect of Artifact compilation. A read-only first
phase captures every parent directory device/inode and, for existing targets,
device/inode, mode, content digest, and bounded content, then returns stable
Rust-computed hunk IDs without creating an approval. The submitted review digest
and per-file selection are recomputed and validated by Rust; Rust reconstructs
the selected files and binds only that exact result to the operation. Dispatch
rechecks the selected set and current Artifact revision before the in-process
Rust executor stages every member. Creation uses no-replace semantics;
replacement retains a private base backup; a later failure rolls back already-
installed members in reverse order. Uncertain install, rollback, or cleanup
verification is reported as `UnknownExecution`.

The same executor now writes a bounded, atomically replaced and fsynced private
manifest before creating deterministic stage and backup entries. A fully staged
manifest is the durable boundary before any target install. The terminal
`committed` or `rolled_back` manifest remains until the daemon operation receipt
is durable, closing the crash window between filesystem recovery and the
authority journal. Gateway startup classifies targets by device, inode, mode,
and digest, completes an exact commit or rollback when possible, and leaves
ambiguous external changes untouched. Ambiguity blocks later Workspace Apply
operations but not Terminal sessions or read-only Agent interaction. Tier 2
result acceptance reuses this durable executor for exact byte additions,
modifications, and deletions; the Artifact editor remains a text-only workflow.

## Approval and escalation

Approval controls whether a bounded capability may be issued. It does not turn
off the sandbox.

An approval view includes:

- the exact command or opaque effect identity;
- actor and provider;
- cwd, workspace/worktree identity, and executable digest when known;
- new read, write, network, secret, device, socket, or GUI capabilities;
- base and proposed profiles, enforcement tier, and duration;
- whether the effect is locally transparent or opaque;
- the operation revision and the consequence of allowing once, for the turn,
  or for the session.

Changing command, cwd, executable, resolved path set, network destination,
profile, backend, operation revision, or accepted input digest invalidates the
lease and requires re-evaluation. A session grant may reduce repetitive prompts
but every dispatch still receives a newly bound lease.

Command allowlists and deny-lists may inform whether approval is required. They
are not sufficient enforcement because shell composition, aliases,
interpreters, child processes, and mutable executables can change behavior.
The strictest applicable deny rule wins, and complex shell syntax that cannot be
analyzed safely is treated as opaque rather than optimistically decomposed.

## Agent, ACP, MCP, and Codex integration

Hyper Term distinguishes the Agent control process from the effects it proposes.

- a proposal-only ACP/MCP/agent driver gets a minimal driver profile and no
  ambient workspace write authority;
- a shell, tool, or Computer Use request becomes a new Hyper Term operation and
  is dispatched through the corresponding Rust-owned driver;
- a local third-party agent that performs effects internally is opaque and must
  run inside Tier 2 or an equivalently strong outer sandbox;
- an MCP tool name such as `shell`, `filesystem`, or `computer` grants no
  capability by itself;
- remote tools are authorized as opaque effects with declared scope and
  `UnknownExecution` semantics on uncertain completion.

The local Codex source exposes a `PermissionProfile::External` mode in which an
external caller owns filesystem isolation while Codex retains an explicit
network policy. This is the preferred long-term integration shape:

1. Hyper Term remains the filesystem and process authority;
2. Codex app-server approval requests are normalized into Hyper Term Operations;
3. Codex network access is restricted and routed through a Hyper Term-owned
   proxy or an equivalently reconciled policy;
4. duplicate inner and outer filesystem sandboxes are avoided when the
   app-server protocol exposes the required configuration;
5. if the protocol cannot express external enforcement or Codex performs opaque
   effects, the complete Codex workload runs in Tier 2 instead.

The existing Codex adapter does not make an app-server approval authoritative.
It is a proposal adapter. Support for external enforcement must be confirmed in
the negotiated app-server/runtime surface before implementation.

### Implemented macOS control-process boundary

The macOS baseline now applies the same Rust-compiled Tier 1 Seatbelt and
managed-network policy to direct Codex and every configured ACP adapter:

- the provider executable, adapter runtime, interpreter, dynamic libraries,
  PATH toolchains, and provider-specific preference/authentication roots are
  explicit read roots;
- the live workspace is read-only to the provider process tree; only a private
  per-session home, tool home, and scratch root are writable;
- `HOME`, `CODEX_HOME`, and `TMPDIR` are session-private for Codex ACP. Its
  authentication file and read-only preferences are staged without copying
  credentials into logs or persisted policy;
- optional MCP access is limited to the broker control socket and the
  digest-pinned MCP executable;
- the MCP connector is proposal/proxy-only inside this process tree. Pinned
  Deno, compiler, WASM, workspace snapshot, cache, and scratch paths remain in
  the outer Rust daemon, which applies their separate narrow Seatbelt after an
  operation-bound execution request;
- outbound sockets are denied except for an authenticated loopback CONNECT
  proxy with a provider-specific hostname allowlist and port 443;
- RFC 2544 `198.18.0.0/15` results are accepted only after the CONNECT hostname
  and port pass policy, for macOS transparent proxies that implement Fake-IP
  DNS. IP literals, RFC 1918, loopback, link-local, documentation, and other
  reserved targets remain denied.

The negative conformance test completes a real ACP initialize/session handshake
while proving that the same process cannot read an unrelated host secret or
write the workspace. A local installed `codex-acp` smoke test also completes
through the Seatbelt and managed proxy boundary.

This is a Tier 1 control-process boundary, not the deferred Tier 2 environment.
Homebrew adapters may read the Homebrew installation root because their Node
interpreter and dylibs cross Cellar and `opt` paths. Opaque provider-internal
execution, hermetic dependencies, resource isolation, and review-only
acceptance still require Tier 2; protocol tool requests continue through the
Rust broker instead of inheriting workspace write authority.

### Implemented Tier 2 Lima execution baseline

`hyper-term-sandbox` owns the Tier 2 source and execution lifecycle. It creates
and destroys a private detached Git worktree for one exact source commit. It
does not copy the user's live working tree, so tracked dirty edits and untracked
files never enter the environment. The source repository and private state root
cannot contain one another, task identifiers are path-safe, environment names
are digest-derived, and a collision fails closed.

The manager asks Git only to register a `--no-checkout` worktree and populate
its index. Rust then reads the commit tree and blobs directly, with bounded file
count, per-file size, total size, and Git diagnostic output. This intentionally
does not run checkout filters, smudge commands, repository hooks, or filesystem
copies from the user's checkout. Tree paths and modes are validated; submodules
and unsupported entries fail closed, while symlinks are accepted only when
their lexical target remains inside the isolated worktree. A test installs a
hostile checkout hook and smudge filter and proves neither executes.

Each environment has a mode-0600 manifest binding the source repository, full
commit ID, task, worktree path, bounded inventory, byte count, and inventory
digest. Explicit cleanup rereads that manifest, validates the exact state-root
relationship, removes the registered worktree, and preserves unrelated dirty
source state. Tests cover clean commit identity, source/worktree independence,
private manifests, unsafe identifiers and nested roots, escaping symlinks,
failed-creation cleanup, and worktree registry cleanup.

An approved shell operation that explicitly requests
`sandbox.isolated_task` is compiled as `SandboxEnforcement::IsolatedTask` with
the `LimaVm` backend. It cannot be sent to the host PTY path. The daemon consumes
the same revision-bound, one-use capability lease used by Tier 1 before it
materializes the exact commit or starts a VM.

The Lima runner accepts only a local image with a caller-supplied lowercase
SHA-256. Rust copies that image into the mode-0700 environment while hashing it,
then gives Lima the private copy, removing the verify/use replacement window.
Every task receives its own `LIMA_HOME`, digest-derived instance name, bounded
CPU, memory, disk, wall time, process count, and output budget. Containerd,
proxy propagation, host DNS, SSH agent forwarding, X11, and non-SSH port
forwarding are disabled. The only host mount is the exact-commit worktree at
`/workspace`.

The VM needs a network while Lima boots and establishes its control channel,
but the approved command is executed under Linux `unshare --net` with a cleared,
fixed environment. Therefore task code has neither the VM boot network nor host
environment variables. Rust passes argv without shell interpolation, streams
bounded stdout and stderr, distinguishes non-zero exit, signal, timeout, and
cancellation, and runs `stop --force` plus `delete --force` on every exit path.
A cleanup failure suppresses the otherwise valid result and fails closed.

After execution Rust inventories tracked and untracked changes without
following symlinks, bounds file count and bytes, and records per-file content
digests plus an aggregate inventory digest. The daemon retains the isolated
result in daemon-private durable state and exposes digest-bound file reads plus
explicit discard; reopening the daemon revalidates the manifest, receipt, and
live Git inventory before restoring a completed result.

Applying a result is a separate `FileEdit` operation. Rust prepares an atomic
workspace transaction against the current file identities, binds its digest to
the source operation and Tier 2 inventory, and asks for another human
permission decision. A successful task therefore never implies acceptance.
Only the approved transaction can write the workspace; stale targets roll back,
and the existing durable transaction journal handles crashes after application
starts. The acceptance slice supports bounded regular-file byte additions,
modifications, and explicit deletions. Valid UTF-8 content receives the bounded
Rust-generated Diff; non-UTF-8 content is represented canonically as base64 in
the operation-bound proposal and projected only as byte counts plus SHA-256,
never as a fake textual patch. Binary base contents are not copied into the
acceptance log: Rust retains only their bounded size and filesystem identity,
then rechecks their device, inode, mode, and digest before installation or
recovery. A deletion is a tombstone in the reviewed transaction, not an
empty-file write: Rust hard-links the exact reviewed base as a private rollback
backup, atomically unlinks only the matching device/inode, and restores the
backup after an interrupted partial transaction. Type changes still fail
closed.

Unit tests exercise success, non-zero exit, cancellation, timeout, output
flood, cleanup, dirty-worktree separation, durable result reopen, digest-bound
reads, review inventory, rejection before acceptance approval, and exact
transactional apply. On macOS the generated configuration is also validated by
the installed Lima 2.1.1 parser without booting a VM. The opt-in real backend
test has additionally passed with Lima's digest-pinned Alpine 3.23.3 aarch64
cloud image: it boots the VZ guest, proves the command UID is non-zero, proves
an external network request fails inside the task namespace, inventories the
generated worktree file, and verifies that stop/delete leaves no hostagent or
VM process. The test remains opt-in because the 225 MiB image must be supplied
locally and is never downloaded by the product or normal test suite.

If the daemon restarts before a receipt is durable, it derives the same short
private Lima home and instance name from the environment identity, forces the
instance through stop/delete, removes the incomplete worktree registration,
and only then discards the operation directory. A missing Lima executable or a
failed delete blocks that recovery instead of silently abandoning a live VM.

The packaged desktop supervisor exposes this backend through the complete
`--lima`, `--lima-image`, and `--lima-image-sha256` tuple, with equivalent
`HYPER_TERM_LIMA_*` environment variables for managed launches. Partial
configuration fails before the Native renderer starts. Rust validates the
explicit `limactl` executable and version probe, requires an absolute local
image, constructs the bounded VZ profile, and passes the runner to the Agent
gateway. Only then does ACP initialization advertise its Terminal client
capability. Without that runner, `terminal/create` remains unadvertised rather
than falling back to the ordinary user-owned PTY or an unenforced host process.

This remains an experimental Tier 2 baseline. The Agent gateway now recovers
retained results, exposes a bounded side-effect-free Diff preview, and lets the
Native review card request the separate workspace-apply permission only after
the user has read that preview. Opening the Diff neither creates an operation
nor mints a permission. The card labels deletion inventory, renders
Rust-generated text hunks, and shows bounded binary size plus a short SHA-256
identifier before approval. A release-gated boot test using the production
pinned image is still open.
Opaque ACP provider workloads also remain on the Tier 1 control-process path
until their credentials, dependencies, and broker channels can be staged into
this environment without broadening its mounts.

## Audit and observability

Every execution records a `SandboxReceipt` containing at least:

- operation and revision;
- actor, driver, executable identity, command digest, and cwd;
- source workspace/worktree revision;
- canonical profile and compiled policy digests;
- backend type and version;
- lease scope and approval correlation without secret values;
- start, exit, cancellation, timeout, violation, and supervision-loss state;
- bounded resource and network summaries;
- produced diff/artifact/evidence digests;
- whether the effect was transparent, sandboxed opaque, remote opaque, or
  unsandboxed user authority.

Raw terminal, tool, network, and file content remains off by default in the
audit log because it may contain secrets or private source. Metadata and
content hashes are preferred. Optional capture requires explicit retention and
redaction policy.

The UI displays the active profile and enforcement tier for every Agent
terminal. A sandbox badge is evidence from the Rust receipt, not a UI-selected
label.

## Fail-closed rules

Dispatch is rejected when:

- the selected backend is unavailable or cannot enforce the requested policy;
- path normalization, glob expansion, mount construction, or protected carve-out
  enforcement is incomplete;
- the executable, command, operation revision, or profile digest differs from
  the approved lease;
- direct network bypass cannot be blocked for a proxy-only profile;
- the requested cwd is not visible with the required access;
- required runtime roots would implicitly expose a denied parent or secret;
- a capability lease is expired, consumed, belongs to another actor, or targets
  another operation;
- an opaque process requests a Tier 1 profile that cannot bound its effects;
- resource or policy limits cannot be installed before the child executes.

Fallback to a stricter profile is allowed only when the operation can still
function and the resulting profile digest is recorded. Fallback to broader
authority is never automatic.

## Reference assessment

### Codex

The local `/Users/phodal/ai/codex` snapshot at commit
`56395bddaf26eb2829387ca6a417bf9128e5b239` is the primary implementation
reference. Relevant patterns include:

- managed, disabled, and externally enforced permission profiles;
- per-command additional filesystem and network permissions;
- built-in read-only, workspace-write, and danger-full-access profiles;
- `Read`, `Write`, and `Deny` filesystem entries and protected repository
  metadata;
- a platform-neutral sandbox manager with macOS Seatbelt, Linux
  bubblewrap/seccomp, and Windows restricted-token backends;
- read-only-by-default mounts with writable roots and protected subpath
  carve-outs;
- managed network policy separated from filesystem enforcement;
- fail-closed handling for platform policies that cannot be represented.

Codex is Apache-2.0. Hyper Term may reuse compatible code subject to normal
dependency, notice, provenance, and coupling review. The preferred approach is
to own the canonical Hyper Term protocol and port the smallest necessary policy
compiler/backend behavior rather than importing the entire Codex runtime graph.

Primary references:

- [Codex permission profiles](https://learn.chatgpt.com/docs/permissions)
- [Codex sandboxing](https://learn.chatgpt.com/docs/sandboxing)
- [Codex agent approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security)
- local `codex-rs/protocol/src/models.rs`
- local `codex-rs/protocol/src/permissions.rs`
- local `codex-rs/sandboxing/src/manager.rs`
- local `codex-rs/sandboxing/src/seatbelt.rs`
- local `codex-rs/sandboxing/src/seatbelt_base_policy.sbpl`
- local `codex-rs/linux-sandbox/src/bwrap.rs`

### Warp

The local `/Users/phodal/ai/warp` Git object snapshot at commit
`0017f3059a4ca705c2b716f2c44ab9761b24c2b0` is a behavioral and product-design
reference. The worktree checkout was incomplete during this audit, so findings
were read from the fixed Git object and checked against Warp's public docs.

Useful patterns are:

- an explicit Docker Sandbox terminal rather than silently changing every new
  terminal;
- per-instance sandbox identity and owner-only host scratch directories;
- a dedicated empty primary workspace that does not expose the current repo or
  home directory by default;
- read-only bootstrap mounts and a container-local `/home/agent`;
- cloud environments that separate setup, Agent execution, result capture, and
  environment destruction;
- visible auto-approve/takeover controls and configurable Agent profiles.

Warp's current Agent allowlist, deny-list, `Agent Decides`, and
`Run Until Completion` behavior is approval/autonomy policy, not proof of OS
containment. `Run Until Completion` can bypass the deny-list, so Hyper Term will
not model it as a sandbox profile.

The audited local Docker Sandbox path also contains unresolved cleanup and host
environment passthrough TODOs. It is not used as the production enforcement
reference.

Only Warp's `warpui_core` and `warpui` crates are MIT; the rest of the repository
is AGPL-3.0. Under ADR 0008, Hyper Term does not copy, link, vendor, or translate
the Warp sandbox implementation. It may independently implement the observable
UX and lifecycle requirements documented here.

Primary references:

- [Warp cloud agent environments](https://docs.warp.dev/platform/environments/)
- [Warp Agent profiles and permissions](https://docs.warp.dev/agent-platform/capabilities/agent-profiles-permissions)
- [Warp repository and licensing](https://github.com/warpdotdev/warp)
- Git object `app/src/terminal/local_tty/docker_sandbox.rs`
- Git object `app/src/terminal/local_tty/unix.rs`
- Git object `app/src/ai/blocklist/permissions.rs`

## Alternatives considered

### Rely only on approval prompts

Rejected. Approval cannot constrain descendants, unexpected build-script
behavior, compromised dependencies, filesystem access, or network exfiltration.
It also creates prompt fatigue for routine operations.

### Infer safety from the command string or model risk label

Rejected. A harmless-looking interpreter, compiler, test, shell function, or
mutable executable may perform arbitrary effects. Model labels remain UI and
policy inputs, never authorization evidence.

### Run every Agent command in Docker

Rejected as the only backend. Containers provide a useful Tier 2 boundary but
add startup, image, filesystem, toolchain, networking, macOS virtualization, and
interactive PTY costs. They do not replace a low-latency native sandbox.

### Apply a sandbox to every terminal

Rejected. It would break the product boundary between a normal user terminal
and an Agent environment, surprise users, and make normal shell behavior depend
on Agent policy.

### Let each Agent own its sandbox and approvals

Rejected. Agent implementations have different semantics and may be remote or
opaque. Hyper Term would lose a single authority, consistent audit, and the
ability to protect other Agent/tool integrations.

### Copy Warp's sandbox implementation

Rejected under ADR 0008 because the relevant application code is AGPL-3.0 and
product-coupled. Its behavior remains useful evidence.

### Import the complete Codex sandbox/runtime graph

Rejected as the initial integration. Although the license is compatible, the
full graph would couple Hyper Term's durable protocol and lifecycle to another
product runtime. Small permissively licensed components may be adopted after
dependency and provenance review.

### Treat Deno permissions as the OS sandbox

Rejected. ADR 0003 already records that runtime permission brokers do not cover
all loader, CLI, FFI, native-addon, or third-party behavior. Deno permissions
remain defense in depth inside an OS boundary.

## Consequences

Positive consequences:

- routine Coding Agent work can run with bounded authority and fewer repetitive
  prompts;
- user approval, policy, OS enforcement, and audit have distinct semantics;
- normal terminal behavior remains unsurprising;
- the same policy can back native, container, daemon, test, and future remote
  adapters;
- third-party agents and MCP servers cannot gain authority merely by naming a
  tool or writing to a PTY;
- exact receipts make sandbox state, escalation, and uncertain execution
  reviewable;
- Codex can integrate without becoming the machine-authority kernel.

Costs and constraints:

- platform backends require substantial adversarial testing and ongoing OS
  compatibility work;
- macOS Seatbelt rules must be smoke-tested across supported OS versions;
- Linux user namespace, AppArmor, seccomp, and container-host combinations need
  capability probes and explicit failure modes;
- Windows enforcement strength varies by backend and may initially support
  fewer split filesystem policies;
- package managers, language servers, debuggers, GUI tools, and interactive
  programs need curated runtime profiles;
- filesystem and network deny rules introduce compatibility failures that must
  surface as actionable policy diagnostics, not be bypassed;
- Tier 2 requires image, cache, cleanup, quota, and artifact-acceptance
  lifecycle management.

## Delivery plan

### Phase 0: protocol and test harness

- add canonical sandbox, additional-permission, lease, receipt, and violation
  types to `hyper-term-protocol`;
- add deterministic policy reduction and digest fixtures to `hyper-term-core`;
- define `SandboxLauncher` and `NetworkCapabilityBroker` ports;
- add a fake backend that proves revision, digest, one-use lease, fail-closed,
  and state-transition behavior without claiming OS isolation;
- keep the current production spawn path unchanged until a real backend passes
  the enforcement gates.

### Phase 1: macOS Tier 1

- implement the `hyper-term-sandbox` crate and Seatbelt profile compiler;
- launch both non-PTY commands and interactive PTYs inside the compiled profile;
- implement read-only runtime roots, writable workspace/scratch roots, protected
  metadata, cleared environment, process-tree supervision, and network-off;
- add a profile inspection/debug command that exposes normalized rules and
  digest without exposing secrets;
- switch only explicit Agent terminal operations to the sandboxed port.

### Phase 2: Linux Tier 1

- implement bubblewrap mount construction, user/PID/network namespaces, minimal
  `/dev`, fresh `/proc`, seccomp, and `NO_NEW_PRIVS`;
- probe backend support at startup and reject unsupported split profiles;
- keep a compatibility path only when it can prove a strictly narrower policy.

### Phase 3: managed network and scoped escalation

- add the Rust-owned proxy, domain/port policy, private-network blocking, DNS and
  redirect validation, and metadata-only audit;
- add one-operation, turn, and session grant inputs that mint exact one-use
  leases;
- separate dependency setup/install operations from offline Agent execution.

### Phase 4: Tier 2 environments

- qualify the experimental Lima/VZ backend with a production pinned image,
  restart recovery, bounded patch export, and release conformance tests;
- evaluate whether a container backend is also useful for faster lower-risk
  workloads without weakening the Lima isolation contract;
- run opaque third-party Agent workloads in Tier 2 by default.

### Phase 5: Windows and provider reconciliation

- implement and qualify restricted-token and stronger isolated-user backends;
- reconcile Codex external enforcement and managed network behavior through the
  negotiated app-server surface;
- add per-provider conformance fixtures without allowing provider-specific
  permissions to mutate canonical policy.

## Validation gates

No backend is called enforced until automated integration tests prove its
negative boundaries with real child processes.

### Protocol and policy

- equivalent profiles compile to stable canonical digests;
- malformed, conflicting, cyclic, overflowing, or unsupported policies fail
  closed;
- deny precedence and protected carve-outs survive profile composition;
- a changed operation revision, command, cwd, executable, path set, or profile
  invalidates approval and lease;
- one-use, expiry, actor, and scope checks reject lease replay;
- reducers distinguish `Failed`, `Denied`, `Cancelled`, `Violated`, and
  `UnknownExecution` without silent replay.

### Filesystem

- read-only cannot write the workspace;
- workspace mode cannot write outside authorized roots;
- `.git`, `.hyper-term`, Agent instructions, and denied secret paths remain
  protected under a writable parent;
- scratch and approved cache roots remain writable;
- `..`, absolute paths, symlinks, dangling symlinks, rename races, hard links,
  globs, newly created protected paths, and nested writable carve-outs cannot
  widen access;
- device nodes and Docker/SSH/GPG/desktop sockets remain unavailable unless
  explicitly brokered;
- package setup cannot mutate an unaccepted live dependency state.

### Network

- offline mode blocks IPv4, IPv6, UDP, direct DNS, loopback, private/link-local,
  Unix sockets, and child-process bypass;
- proxy-only mode reaches allowed domains and rejects denied domains, IP
  literals, redirects, DNS rebinding, private targets, and alternate ports;
- proxy credentials and response bodies do not enter default logs or terminal
  environment dumps;
- a backend that cannot block direct sockets rejects proxy-only execution.

### Process, PTY, and resources

- descendants inherit restrictions across shells, interpreters, compilers, and
  exec chains;
- cross-sandbox `ptrace`, signals, process inspection, privilege escalation, and
  host namespace access are blocked where the profile requires it;
- controlling PTY, resize, job control, `Ctrl-C`, UTF-8/CJK, truecolor, exit
  status, and final-output drain match the normal terminal contract;
- cancellation terminates the complete process tree without killing unrelated
  terminal sessions;
- process, CPU, memory, open-file, disk, output, and wall-time limits produce
  bounded structured outcomes;
- forged terminal output and OSC sequences cannot change authority or receipts.

### Tier 2 and acceptance

- the environment cannot see the user's home or unrelated workspaces;
- image, source, cache, policy, and toolchain digests are recorded;
- cleanup occurs after success, failure, timeout, cancellation, and app restart;
- only declared regular files, diffs, and content-addressed artifacts can be
  accepted;
- symlinks, sockets, devices, ownership changes, permission surprises, and
  undeclared paths block acceptance;
- applying an accepted diff is a separate exact operation and preserves
  unrelated dirty-worktree changes.

### Cross-platform and release

- run the negative conformance suite on every supported OS and backend;
- probe OS/backend capability at runtime and report the exact unsupported rule;
- pin or record backend binaries, generated policy schema, and provenance;
- include supported OS-version smoke tests for Seatbelt and Windows profiles;
- run `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` for each Rust protocol or lifecycle change;
- run the Workbench Deno checks when approval, profile, receipt, or violation UI
  projections change.

## Deferred decisions

The following choices require implementation spikes or separate ADRs:

- the final serializable schema and profile configuration syntax;
- whether the current Lima/VZ baseline remains the macOS default or is joined
  by a lower-latency container/native-VZ backend;
- the exact Windows minimum supported enforcement level;
- the managed network proxy implementation and TLS inspection policy;
- resource-limit mechanisms and defaults per platform;
- which language/toolchain runtime profiles ship as reviewed presets;
- retention and recovery policy for crashed Tier 2 environments;
- whether any part of Codex's Apache-2.0 sandbox crates is imported, extracted,
  or independently reimplemented;
- the app-server negotiation required to select Codex external enforcement.

These decisions may refine the backend without changing the authority model:
Rust compiles policy, issues the exact lease, launches the boundary, supervises
the effect, and records the receipt.
