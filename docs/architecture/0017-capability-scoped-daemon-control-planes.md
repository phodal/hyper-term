# ADR 0017: Separate daemon control planes with capability-scoped authority

- Status: proposed
- Date: 2026-07-22
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md),
  [ADR 0004](0004-versioned-agentic-ui-artifacts.md),
  [ADR 0009](0009-rust-acp-mcp-adapters.md), and
  [ADR 0014](0014-rust-owned-coding-agent-sandbox.md)
- Related to: [ADR 0015](0015-native-scene-snapshots.md) and
  [ADR 0016](0016-operation-scoped-execution-contexts.md)
- Refines: client identity, MCP broker access, event visibility, and human
  approval evidence in ADR 0002, ADR 0009, and ADR 0014, plus generated-artifact
  review evidence in ADR 0004

## Context

Hyper Term deliberately separates trusted machine authority from untrusted
providers and presentation surfaces:

- Rust owns PTYs, process lifecycle, permissions, operation revisions,
  execution, durable receipts, and accepted artifacts;
- an Agent, ACP adapter, MCP connector, model response, terminal stream, or
  generated UI may propose an action but must not grant itself authority;
- the Native desktop is the trusted user-interaction client;
- WebViews render bounded projections and never receive a generic process or
  filesystem bridge.

The current daemon control protocol does not yet enforce those trust domains at
the connection boundary. One Unix socket accepts a `Hello` containing a
caller-supplied `ClientId` and protocol version. Once the version is accepted,
the connection may send the complete `ControlRequest` vocabulary, including:

- task creation and block snapshots;
- operation proposal, permission decision, begin, execution, completion, and
  artifact acceptance;
- ordinary user-shell creation;
- terminal subscription, resize, close, and input-lease acquisition.

The server validates operation revisions and resource identity inside many
individual handlers, but it does not attach a trusted role, task scope, or
request allowlist to the connection itself. A self-declared `ClientId` is used
for input-lease ownership and cleanup, not authentication.

This becomes an authority bypass when brokered MCP is enabled. The provider
process tree receives Seatbelt permission to connect to the daemon control
socket so that its configured stdio MCP connector can reach the Rust broker.
The provider also receives the connector command and arguments, including the
socket path and task identity. A hostile or compromised provider can therefore
connect directly, impersonate its MCP child, and use requests that were intended
only for the trusted desktop or daemon-owned executors.

The shortest current bypass does not require an operation or approval:

```text
contained provider
  -> connect to general control socket
  -> Hello with a new self-selected ClientId
  -> OpenUserShell
  -> AcquireInputLease for that terminal and ClientId
  -> TerminalInput
```

The same connection can attempt to decide permissions, begin or complete
operations, read task projections, or observe the global event stream. Some of
those requests will still fail state and revision checks, but failure depends on
the target object's current state rather than the caller's authority.

This is not fixed by trusting the connector binary digest. The provider is the
parent that launches the connector and has the same allowed socket path. It may
send the protocol itself without modifying the pinned child.

The human approval projection has a related integrity gap. The durable event
contains the typed `OperationAction`, and execution is bound to its digest and
revision, but `BlockPayload::Operation` exposes only a summary. The approval
block contains a generic prompt and decision options. For a shell request, the
summary loses argv boundaries and omits cwd and environment. For an MCP request,
the UI sees a profile description and digest but not the bounded canonical
arguments. The prompt currently asks whether to allow the "exact operation"
without displaying enough trusted evidence to review that exact operation.

The transport boundary and the approval boundary must agree:

> A caller may invoke only the authority assigned by the daemon, and a user may
> authorize only an operation whose effective details were rendered from the
> same immutable revision and digest that execution will consume.

## Decision drivers

1. Rust remains the only machine-authority and permission-decision boundary.
2. A provider and every descendant in its sandbox are untrusted, including a
   process that can imitate a configured MCP connector.
3. Client identity and role are assigned by the accepting server endpoint, not
   asserted by `Hello`, environment, argv, executable name, or model output.
4. A compromised least-privilege connector must still be unable to approve an
   operation, open a user shell, acquire terminal input, read unrelated tasks,
   or execute an arbitrary broker request.
5. Task, provider session, operation, tool, digest, revision, expiry, and event
   visibility are bound server-side wherever they are known at connection
   creation.
6. The general desktop control protocol and Agent/MCP proposal protocol must be
   separate review surfaces even when both use local Unix transport.
7. Filesystem permissions, peer credentials, unguessable paths, and tokens are
   defense in depth. None substitutes for a server-side request allowlist.
8. Human approval must show a bounded, typed, redacted projection derived from
   the exact action that will be dispatched.
9. Secret values never enter the renderer, operation summary, approval detail,
   journal, diagnostic output, or ordinary action digest.
10. Revocation, reconnect, daemon restart, provider restart, and uncertain
    execution preserve explicit lifecycle semantics and never broaden access.
11. Existing desktop, terminal browser, provider, and release flows migrate
    incrementally but fail closed when client authority cannot be established.
12. An AI-generated UI is not promoted as review-ready merely because it
    compiles. Its real isolated render must produce bounded visual-quality
    evidence without turning a subjective aesthetic score into authority.

## Threat model

### Protected assets

This decision protects:

- permission decisions attributed to the user;
- ordinary user shells and their input leases;
- Agent-owned Tier 2 terminals and reviewed workspace results;
- task, operation, block, terminal, artifact, and receipt data belonging to
  another session;
- the ability to begin, dispatch, complete, cancel, or resolve an operation;
- the exact action, revision, profile, and result identity authorized by the
  user;
- control-plane event streams and snapshots that may reveal private prompts,
  paths, tool activity, or approval state;
- daemon availability from unbounded proposal or subscription abuse.

### Untrusted principals and inputs

The design treats these as untrusted:

- direct Codex and local ACP provider processes;
- provider-launched stdio MCP connector processes;
- any descendant of those processes, even when the executable was digest
  pinned before launch;
- MCP request IDs, tool names, arguments, annotations, roots, and reported
  capabilities;
- ACP permission messages and provider-reported actor labels;
- terminal output, OSC sequences, model text, generated source, and WebView
  content;
- another same-user process that discovers a socket path or copies a stale
  capability;
- a stale trusted client from a prior daemon or renderer generation.

The Native desktop supervisor and daemon are trusted product components. A
complete compromise of either can counterfeit user intent and is outside this
ADR's isolation claim, but their protocol interactions remain bounded and
auditable so accidental misuse fails closed.

### In-scope attacks

The implementation must block:

- a provider opening an ordinary user-authority shell;
- a provider acquiring an input lease or sending raw terminal input;
- a provider submitting a permission decision that is recorded as
  `Actor::User`;
- a connector creating or selecting a task outside its assigned session;
- cross-task, cross-provider, cross-operation, cross-tool, or stale-revision
  replay;
- a connector invoking a generic `BeginOperation`, `CompleteOperation`, or
  `AcceptGenUiArtifact` to manufacture a state transition or receipt;
- event or snapshot access outside the connection's assigned scope;
- reuse of a capability after provider stop, session replacement, daemon
  restart, expiry, or explicit revocation;
- treating socket mode, process ancestry, executable digest, `ClientId`, or a
  client-supplied role string as sufficient authorization;
- approving one displayed action and dispatching another action whose argv,
  cwd, environment binding, MCP arguments, executable, profile, or digest
  differs;
- hiding material action details behind an unreviewable generic summary.

### Explicit limitations

- A provider may observe the command used to launch its stdio MCP connector.
  The design does not assume that a connector socket path or credential remains
  secret from its parent provider.
- Unix peer credentials can prove an operating-system user, not which product
  trust domain a same-user process belongs to.
- An allowed MCP tool may still be internally opaque. This ADR limits who can
  request and dispatch it; ADR 0014 and ADR 0016 govern its process, network,
  filesystem, credential, and execution-context authority.
- A trusted desktop compromise can present fraudulent approval UI. Preventing
  that requires platform code-signing, update, and host-integrity work beyond
  this protocol decision.
- Root, kernel, debugger, code-injection, and physical attacks are outside this
  local application boundary.

## Decision

Hyper Term will replace ambient access to one general control socket with
**server-assigned connection authority and separate control planes**.

The target architecture has four distinct paths:

```text
trusted Native desktop
  -> Desktop Control Plane
  -> user shells, approval decisions, scoped projections

provider process
  -> ACP/Codex protocol handled by Rust driver
  -> proposal events only

provider-launched MCP connector
  -> Agent Capability Plane bound to one provider session and task
  -> one bounded MCP proposal/result exchange

Terminal WebView
  -> token-bound loopback Terminal Gateway
  -> one terminal attachment and input lease
```

The Agent Capability Plane is not a restricted convention layered on the
general `ControlRequest` enum. Its final form is a separate, narrow protocol
and listener whose accepted messages cannot express desktop authority.

### Trust domains and server-assigned roles

Every accepted connection carries an internal `ConnectionAuthority` selected
by the daemon from the listener and launch record that accepted it. Conceptually:

```rust
enum ClientRole {
    DesktopController,
    AgentMcpConnector,
    AdministrativeCli,
}

struct ConnectionAuthority {
    role: ClientRole,
    daemon_instance: Uuid,
    client_generation: u64,
    task_id: Option<TaskId>,
    provider_session_id: Option<ProviderSessionId>,
    connector_instance_id: Option<ConnectorInstanceId>,
    allowed_requests: RequestSet,
    event_scope: EventScope,
    expires_at: Option<Instant>,
    revoked: bool,
}
```

This is an internal enforcement object, not a serializable bearer capability.
The client may still send a `ClientId` for correlation, input sequencing, and
diagnostics, but the daemon never derives authority or actor identity from it.

`Actor::User` is available only to an authenticated trusted desktop decision
path. An ACP, model, MCP, connector, driver, automation fixture, or generic CLI
request cannot choose that actor.

### Separate listeners and runtime roots

The trusted desktop and administrative CLI use a private control endpoint under
the daemon state root. Agent sessions never receive access to that endpoint.

Each provider session that needs brokered MCP receives a dedicated capability
endpoint created under its private runtime root. The listener is bound in
advance to:

- the current daemon instance;
- one task;
- one provider session and generation;
- one connector/runtime identity;
- the exact brokered tool catalog available to that session;
- a bounded lifetime and revocation handle.

Seatbelt grants the provider process tree access only to that dedicated path.
The general control socket is removed from `allowed_unix_sockets`.

The provider can impersonate its child connector and reach the dedicated
endpoint. That is acceptable because the endpoint grants only proposal
authority. Its safety does not depend on distinguishing the provider from the
connector within the same sandbox.

The state root and all control-plane parent directories are created with mode
`0700`; socket files and capability handoff files use mode `0600`. Existing
paths with broader permissions are rejected or migrated explicitly. Peer UID,
path ownership, non-symlink validation, daemon instance identity, unguessable
runtime paths, and short-lived tokens are used as additional checks.

### Narrow MCP capability protocol

The provider-facing endpoint accepts only a bounded MCP call exchange. A
conceptual request contains:

```rust
struct BoundMcpToolProposal {
    connector_request_id: ExternalRequestId,
    tool_name: String,
    canonical_arguments: BoundedJson,
    proposal_digest: Sha256Digest,
}
```

`task_id`, provider session, connector identity, effective tool catalog, and
event scope are taken from `ConnectionAuthority`, not supplied by the caller.
Rust independently recomputes the proposal digest and rejects tools outside the
session catalog.

After receiving a valid proposal, the daemon owns the complete state machine:

```text
bound MCP proposal
  -> create immutable Operation
  -> policy check
  -> publish trusted ApprovalDetail
  -> authenticated desktop decision
  -> begin Dispatching
  -> execute through Rust-owned broker
  -> accept any verified artifact
  -> record receipt and terminal outcome
  -> return the bounded MCP result
```

The connector does not receive generic authority to call `CreateTask`,
`DecidePermission`, `BeginOperation`, `ExecuteBrokeredMcpTool`,
`CompleteOperation`, or `AcceptGenUiArtifact`. Those become daemon-internal
steps for this flow.

During migration, if the existing control protocol is temporarily reused, its
server-side `AgentMcpConnector` matrix is:

| Request | Connector authority during migration |
| --- | --- |
| `Hello` | Correlation only; role is server assigned |
| `ProposeOperation` | MCP tool only, bound task/catalog/digest |
| authority events | Bound operation and task only |
| cancel pending proposal | Own pending request only |
| `CreateTask` | Denied |
| `DecidePermission` | Denied |
| `BeginOperation` | Denied; daemon internal |
| `ExecuteBrokeredMcpTool` | Denied; daemon internal |
| `CompleteOperation` | Denied; daemon internal |
| `AcceptGenUiArtifact` | Denied; daemon internal |
| `OpenUserShell` | Denied |
| terminal subscribe/resize/close/input lease/input | Denied |
| task/block snapshots | Denied except a future explicit redacted own-call view |

The migration allowlist is compiled as an exhaustive match. Adding a new
`ControlRequest` variant fails compilation or tests until every role assigns it
an explicit allow or deny decision.

### Scoped events and backpressure

The general daemon broadcast stream is not forwarded to Agent capability
connections. A connector receives only lifecycle events required for its own
pending requests, identified by the bound task, operation, and connector
request ID.

The server bounds:

- simultaneous pending proposals per connector and task;
- proposal and result bytes;
- event queue bytes and item count;
- permission-wait duration;
- total connector lifetime and idle time;
- reconnect attempts and invalid-request rate.

Overflow, invalid scope, revoked authority, or repeated forbidden requests
close the capability connection and fail pending proposals without changing an
authorized operation into a claim that no execution occurred.

### Reviewable approval evidence

Rust will add a typed `ApprovalDetail` projection derived from the canonical
operation action. It is generated before the permission request and bound to
the same operation revision and action digest.

Conceptually:

```rust
struct ApprovalDetail {
    operation_id: OperationId,
    operation_revision: u64,
    action_digest: Sha256Digest,
    actor: ProposedActor,
    provider: Option<ProviderIdentity>,
    action: ApprovalActionDetail,
    effective_capabilities: Vec<CapabilitySummary>,
    sandbox: EnforcementSummary,
    opaque_effect: bool,
    truncation: Option<TruncationEvidence>,
}

enum ApprovalActionDetail {
    Shell {
        executable: ExecutableIdentity,
        argv: Vec<String>,
        cwd: PathBuf,
        environment: Vec<RedactedEnvironmentBinding>,
    },
    McpTool {
        server: McpServerIdentity,
        tool_name: String,
        canonical_arguments: BoundedJson,
        arguments_digest: Sha256Digest,
    },
    WorkspaceApply {
        source_revision: String,
        targets: Vec<ReviewedTarget>,
        review_digest: Sha256Digest,
    },
    Opaque {
        kind: String,
        identity_digest: Sha256Digest,
        declared_scope: Vec<String>,
    },
}
```

The exact protocol shape may evolve, but these rules are normative:

- argv remains an array; display formatting cannot erase argument boundaries;
- cwd, executable identity, effective sandbox tier, new capabilities, and
  opaque/transparent status are visible;
- ordinary environment values may be shown only when policy marks them safe;
  credentials and authority handles are represented by redacted reference,
  source, audience, and lifetime metadata;
- MCP arguments are canonicalized, size bounded, and paired with their digest;
- a large request uses a bounded preview plus counts and a trusted drill-down
  path to the complete bounded detail. Material fields cannot disappear behind
  an ellipsis;
- if Rust cannot produce a valid reviewable detail for an effect that requires
  approval, the effect cannot enter `WaitingHuman`;
- the Native desktop renders the structured detail as trusted chrome. A
  WebView, provider, or model cannot supply replacement labels or markup;
- a permission decision carries the expected operation revision and approval
  detail digest. Any action or detail change invalidates the decision;
- summaries remain useful timeline text but are not approval evidence.

### AI-generated UI visual quality evidence

GenUI and other AI-generated UI artifacts will pass through a separate
**Visual Quality Gate** after source, compiler, protocol, sandbox, and runtime
safety checks succeed.

This gate answers a product question, not a permission question:

> Does the actual rendered artifact look coherent and usable across the states
> Hyper Term claims to support?

It never grants filesystem, process, network, credential, approval, or desktop
control authority. A visually attractive artifact remains untrusted generated
content inside the isolated preview. A visually weak artifact does not receive
broader capabilities to repair itself.

The artifact lifecycle distinguishes technical acceptance from visual review:

```text
source revision
  -> compile and integrity verification
  -> isolated preview handshake
  -> deterministic visual captures
  -> objective presentation checks
  -> optional advisory aesthetic review
  -> VisualQualityReport
       | objective failure -> NeedsRevision
       | advisory findings -> NeedsReview
       | no blocking issue -> ReviewReady
  -> explicit user acceptance or Agent regeneration
```

`ArtifactAccepted` continues to mean that Rust verified the artifact identity,
source revision, compiler identity, file inventory, and safe preview contract.
It does not mean that the UI is beautiful or ready to ship. Visual quality is a
separate, revision-bound status and receipt.

Every checked revision is rendered from the exact Rust-accepted bundle through
the token-bound packaged Workbench preview path, not reconstructed from source
text, a browser-local rebuild, or a component-tree guess. A `DemoBroker` or
other draft-only preview may help the user iterate, but it cannot issue a host
quality receipt, satisfy this gate, or append runtime evidence as though it ran
the accepted artifact. The artifact identity, accepted bundle digest, preview
runtime identity, and capture manifest must agree before evidence is durable.

The versioned capture matrix includes at least:

- narrow mobile, tablet, and desktop viewports;
- light and dark color schemes when the artifact declares both;
- normal and reduced-motion preferences;
- English, CJK, long-label, and long-content fixtures;
- empty, loading, success, error, disabled, and keyboard-focus states when the
  artifact exposes those states through a bounded data-only scenario contract;
- the default system font fallback and the exact packaged preview runtime.

Missing declared scenarios are reported as coverage gaps. Hyper Term does not
invent imperative test code from model text or grant the artifact a host bridge
to create states.

The deterministic layer checks observable failures such as:

- viewport or document overflow, clipped content, overlap, and hidden primary
  actions;
- unreadable contrast, missing visible focus, undersized interaction targets,
  and keyboard traps;
- broken typography scale, uncontrolled line length, truncated CJK, and
  fallback-font layout shifts;
- inconsistent alignment, spacing-token drift, accidental one-pixel seams,
  and unbalanced container padding;
- missing empty/loading/error feedback, layout instability, and content that
  disappears at a supported viewport;
- unexpected console errors, failed resources, preview timeouts, or long main
  thread tasks during capture.

Objective failures can prevent `ReviewReady`. They remain available as an
isolated repair preview with highlighted evidence rather than replacing the
last known good revision.

A second advisory layer may assess qualities that are not safely reducible to
one deterministic rule:

- hierarchy and visual focal point;
- density, whitespace rhythm, and grouping;
- color harmony and design-token consistency;
- consistency between related components and states;
- whether the result looks unfinished, generic, or visually noisy;
- whether the composition matches the user-provided intent or reference.

Advisory findings produce `NeedsReview`, not an automatic rejection or
acceptance. The generating Agent cannot mark its own output as passed. A local
or remote multimodal reviewer may be used only through an explicit configured
provider and operation. Screenshots, source, prompts, workspace names, and user
content are not sent to a remote reviewer by default.

The host records a bounded, serializable report conceptually shaped as:

```rust
struct VisualQualityReport {
    artifact_id: ArtifactId,
    source_revision: u64,
    artifact_digest: Sha256Digest,
    capture_manifest_digest: Sha256Digest,
    checker_version: String,
    captures: Vec<VisualCaptureEvidence>,
    findings: Vec<VisualQualityFinding>,
    objective_status: ObjectiveVisualStatus,
    advisory_status: AdvisoryVisualStatus,
    reviewer: Option<RedactedReviewerIdentity>,
}
```

Each finding has a stable category, severity, viewport/state identity, bounded
explanation, and optional pixel rectangle or semantic node reference. Captures
are stored locally as content-addressed artifacts; the journal retains their
digests and bounded metadata rather than embedding screenshots.

Changing source, compiler/runtime identity, preview shell, fixture data,
viewport matrix, font inventory, or checker version invalidates the report.
The Workbench shows the report beside the exact revision, lets the user jump to
highlighted evidence, and offers explicit **Accept**, **Keep last good**, or
**Ask Agent to revise** actions. Regeneration creates a new source revision and
must run the gate again.

### Trusted desktop authority

The Native desktop receives its controller authority from the daemon supervisor
through a private launch channel. The credential is never placed in WebView
URLs, JavaScript state, terminal output, model context, provider environment,
or the journal.

The desktop may:

- open an ordinary user shell after explicit user UI intent;
- attach to the terminal selected by the Rust-owned workspace projection;
- acquire input only for that attachment and desktop generation;
- submit a permission decision for the exact displayed operation revision and
  detail digest;
- request bounded task and block projections needed by the visible workspace.

Desktop authority is not equivalent to an unrestricted debug API. Requests are
still validated against the active workspace, visible task, terminal
attachment, expected revision, and daemon instance.

### Capability lifecycle and revocation

A provider-session capability is revoked when:

- the session stops or is replaced;
- the connector process closes;
- its task binding changes;
- its tool catalog or runtime identity changes;
- the daemon restarts;
- the capability expires or exceeds its invalid-request budget;
- the user disables the provider or MCP integration.

Revocation closes the listener and existing streams, removes the socket, and
marks non-dispatched proposals failed or cancelled. If an effect may already
have started, the operation becomes `UnknownExecution` until reconciled. It is
never silently replayed on connector restart.

A reconnect receives a new connection generation and cannot reuse an input
lease, pending request, or authority from the previous generation unless the
daemon explicitly reconstructs one bounded pending proposal from durable state.

### Audit evidence

The daemon records bounded metadata for:

- listener and capability creation, scope, expiry, and revocation;
- accepted client role and connection generation;
- denied request type and denial reason without raw payload or secret values;
- proposal, approval-detail, action, profile, and result digests;
- authenticated user decisions;
- cross-scope, stale-generation, replay, and invalid-request attempts;
- whether execution completed, failed before dispatch, or became unknown after
  dispatch.

Raw capability secrets, socket tokens, complete credentials, terminal bytes,
and unredacted provider payloads are never journaled.

## Component boundaries

### `hyper-term-protocol`

- defines trusted desktop request DTOs, the narrow Agent capability protocol,
  `ApprovalDetail`, role-independent denial codes, and version negotiation;
- does not let a wire client serialize `ConnectionAuthority` or choose an
  effective actor;
- keeps terminal binary input on its existing lease- and sequence-bound frame.

### `hyper-term-core`

- derives reviewable approval detail from canonical actions;
- binds the detail digest, action digest, operation revision, policy result, and
  user decision;
- defines the exhaustive role/request policy and deterministic scope checks;
- owns fail-closed transition semantics independent of Unix transport.

### `hyper-term-sandbox`

- compiles only the dedicated per-session capability socket into the provider
  profile;
- rejects the general control socket in an Agent/provider profile;
- validates private path ownership, non-symlink components, and exact socket
  authority.

### `hyper-term-drivers`

- treats ACP and MCP requests as proposals;
- never constructs a user actor or a desktop authority credential;
- launches only the pinned proposal connector configuration supplied by the
  daemon and remains correct if its parent provider can inspect that config.

### `hyper-term-daemon`

- creates listeners, assigns `ConnectionAuthority`, filters events, enforces
  role/request matrices, executes brokered effects, records receipts, and
  revokes capabilities;
- owns the MCP operation state machine after receiving one bounded proposal;
- creates private state/runtime roots and applies explicit socket modes;
- rejects legacy ambient Agent access to the general control endpoint.

### Native and WebView surfaces

- Native renders `ApprovalDetail` and submits the bound decision;
- the Terminal WebView retains only its token-bound HTTP/WebSocket attachment;
- Workbench and generated previews receive no daemon control capability;
- renderer labels, summaries, or hidden DOM values cannot change the action
  digest or effective connection authority.

## Protocol and migration strategy

The change is intentionally staged so a partial deployment cannot appear safe
while retaining ambient authority.

1. Add a server-side role/request matrix around the existing Unix server and
   tests for every current request variant.
2. Introduce trusted desktop authentication and bind `Actor::User` decisions to
   it. Legacy unauthenticated desktop clients fail with an upgrade-required
   response.
3. Add scoped event filtering and private state/socket permissions.
4. Introduce the separate Agent capability listener and narrow MCP proposal
   protocol.
5. Move MCP begin, execute, artifact acceptance, and completion into one
   daemon-owned state machine.
6. Remove the general control socket from every provider sandbox and launch
   configuration.
7. Add `ApprovalDetail` and require its digest for permission decisions.
8. Delete the temporary connector allowlist on the general protocol after all
   packaged providers use the dedicated endpoint.

The daemon protocol version changes when authentication or request shapes
change. Unsupported old clients are rejected before subscriptions, leases, or
requests are registered. There is no compatibility mode that grants the old
full request vocabulary to an Agent process.

## Failure handling

- Authentication or role establishment failure closes the connection before
  registering event delivery.
- A forbidden request returns a bounded `authority_denied` response, increments
  the invalid-request budget, and never calls the underlying handler.
- Scope mismatch returns the same external denial class without revealing
  whether another task, terminal, or operation exists.
- Permission-detail construction failure prevents `WaitingHuman`.
- Detail digest or operation revision mismatch rejects the user decision and
  refreshes the trusted approval projection.
- Provider or connector disconnect before dispatch fails or cancels the
  proposal according to its current state.
- Disconnect or timeout after dispatch records `UnknownExecution`, not
  `Failed`, until a Rust-owned observation resolves it.
- Revocation is monotonic for a connection generation. A later policy change
  creates a new capability rather than widening the old one.

## Implementation plan

### Phase 0: executable security regression

- Add a contained hostile-provider fixture that receives the current MCP launch
  configuration and attempts the complete general control protocol.
- Prove the current bypass before changing the transport.
- Assert that the fixture can reach its configured proposal endpoint but cannot
  open a user shell, acquire input, decide permission, inspect another task, or
  emit terminal input.

### Phase 1: connection authority and private state

- Add `ConnectionAuthority` and an exhaustive role/request matrix.
- Make user actor attribution a trusted-server decision.
- Require `0700` state/runtime directories and `0600` files/sockets, with a
  tested migration for existing local state.
- Filter events and snapshots by connection scope.
- Add a daemon-lifetime single-writer lock for the state root because private
  permissions do not prevent two same-user daemons from corrupting the journal.

### Phase 2: dedicated Agent capability plane

- Create one scoped listener per provider session.
- Pass only that endpoint in brokered MCP configuration.
- Remove the general control socket from Agent Seatbelt policy.
- Introduce bounded proposal/result messages and daemon-owned operation
  transitions.
- Add expiry, revocation, invalid-request budgets, and cleanup.

### Phase 3: reviewable approvals

- Add protocol and core types for `ApprovalDetail`.
- Project typed shell, MCP, workspace-apply, and opaque details.
- Render them in Native trusted chrome with bounded disclosure for large input.
- Require detail digest plus operation revision on every user decision.
- Remove the generic "exact operation" prompt wherever the exact detail cannot
  be shown.

### Phase 4: AI-generated UI visual quality gate

- Define the versioned capture matrix and bounded data-only scenario contract.
- Capture real isolated preview output for each supported viewport, theme,
  locale/content fixture, motion preference, and declared state.
- Add deterministic layout, overflow, contrast, focus, typography, runtime,
  and stability checks.
- Persist a revision-bound `VisualQualityReport` and render its findings in the
  Workbench with evidence navigation.
- Bind every report and runtime trace to the exact Rust-accepted bundle; label
  browser-local draft rebuilds separately and deny them host quality receipts.
- Keep optional multimodal aesthetic review explicit, redacted, provider-bound,
  and advisory.
- Preserve the last known good artifact when a new revision needs visual work.

### Phase 5: real-provider and release gates

- Run the hostile-provider test inside the real macOS Seatbelt profile.
- Run authenticated Codex, Claude, and Copilot proposal/reject/allow paths.
- Run the strict ACP -> MCP -> approval -> GenUI path through the dedicated
  endpoint.
- Verify daemon, renderer, provider, and connector restart/revocation behavior.
- Add the security suite to pull-request and release workflows.

## Implementation evidence (2026-07-23)

The first Phase 4 environment slices are implemented as objective checker v3:

- Rust owns an ordered six-capture manifest for narrow, tablet, desktop,
  desktop-dark, desktop-reduced-motion, and desktop-focus-first evidence. It
  rejects substituted or reordered environments before persisting a report.
- Each capture receives a new token-bound isolated preview URL with explicit
  color-scheme and motion preferences. The preview fixes `matchMedia` and
  supported CSS preference queries before importing the accepted artifact.
- Reports remain bound to the artifact revision, accepted bundle digest, and
  packaged preview runtime. A digest-valid v1 report is treated as stale and
  triggers recapture instead of blocking the Workbench upgrade.
- The browser verification drives the real Rust Gateway and Deno Workbench
  through all six environments. The focus-first scenario selects the first
  visible keyboard target, requires a real `:focus-visible` style change, and
  records a stable semantic location when the indicator is missing. The gate
  also retains the Deno LSP, keyboard tab, and hostile-preview assertions.

These slices do not claim host-pixel equivalence. Host screenshots,
CJK/long-content fixtures, and declared loading/error/disabled states remain
explicit coverage gaps, so a clean objective report is still `NeedsReview`.

## Validation gates

### Protocol and role enforcement

- every request variant has an explicit decision for every role;
- a caller-supplied `ClientId`, actor, task, role, or socket path cannot widen
  the server-assigned authority;
- unknown protocol versions and legacy unauthenticated clients fail before
  subscriptions or leases exist;
- denial does not reveal cross-scope object existence;
- event delivery contains only the assigned task and pending operations.

### Hostile contained provider

From the real provider sandbox, a test process must be unable to:

- connect to the general desktop control socket;
- create or select another task;
- submit a user permission decision;
- open an ordinary user shell;
- subscribe to, resize, close, or acquire input for a terminal;
- send a terminal binary input frame;
- read another task's Block snapshot;
- begin, execute, accept, complete, or resolve an operation directly.

The same process must still be able to submit one allowed MCP proposal through
its scoped endpoint, receive only its own permission/result lifecycle, and
obtain the Rust-produced result after a genuine desktop approval.

### Scope, replay, and revocation

- changing task, provider session, connector identity, tool, arguments,
  proposal digest, operation revision, or daemon generation rejects the call;
- a capability cannot be reused after stop, expiry, restart, revocation, or
  catalog change;
- invalid-request and event-queue floods remain bounded;
- connector restart never silently replays an already-dispatched effect;
- supervision loss after dispatch yields `UnknownExecution`.

### Approval integrity

- shell argv boundaries, cwd, executable identity, effective capabilities, and
  sandbox tier survive projection exactly;
- secret-bearing bindings remain redacted while their source, audience, and
  lifetime remain reviewable;
- MCP canonical arguments and digest survive projection;
- truncation records original byte/item counts and offers a trusted complete
  bounded view;
- changing any material field invalidates the approval detail digest and prior
  decision;
- a renderer cannot invent, remove, or replace approval fields;
- no operation requiring human approval can dispatch without a matching trusted
  decision for the current detail digest and revision.

### AI-generated UI visual quality

- the exact accepted revision is rendered through the packaged isolated preview
  rather than inferred from source or fixture-only markup;
- the captured artifact id, source revision, accepted bundle digest, preview
  runtime identity, and capture manifest match before evidence is recorded;
- a `DemoBroker`, browser-local rebuild, or stale preview cannot satisfy the
  gate or write durable runtime evidence for a Rust-accepted artifact;
- narrow, tablet, and desktop captures detect overflow, clipping, overlap, and
  hidden actions;
- English, CJK, long-content, theme, reduced-motion, focus, empty, loading, and
  error fixtures are either exercised or reported as explicit coverage gaps;
- contrast, focus visibility, target size, console errors, resource failures,
  layout instability, and capture timeouts produce deterministic findings;
- known deliberately broken fixtures fail with stable categories and evidence
  locations, while known reference fixtures remain below the blocking budget;
- an advisory aesthetic result cannot auto-accept, execute, publish, or widen
  an artifact's capabilities;
- the generating Agent cannot submit or alter the host-owned quality status;
- remote visual review is off by default and requires explicit provider choice,
  data disclosure, and an operation-bound receipt;
- changing source, runtime, fixtures, viewport matrix, fonts, or checker version
  invalidates prior evidence;
- a failed new revision never replaces the last known good artifact.

### Filesystem and local transport

- new and migrated state roots are `0700` and sensitive files/sockets are
  `0600`;
- symlink, ownership, stale-socket, path-substitution, and second-daemon cases
  fail closed;
- the provider profile contains the dedicated capability socket and never the
  desktop control socket;
- another local account cannot read state or connect;
- same-user access without the correct endpoint scope and generation cannot
  obtain additional authority.

### Release

- deterministic fixtures cover allow, reject, cancel, timeout, disconnect, and
  unknown-execution outcomes;
- the real Terminal path still opens a normal user shell only from trusted user
  intent;
- the real ACP/Codex and brokered MCP paths complete without ambient control
  access;
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` pass;
- Native approval projection checks, Deno checks, and the real browser/host
  gates run when protocol or UI fields change.

## Consequences

### Positive

- The core product claim becomes enforced at the server boundary: a provider
  can propose but cannot grant or execute desktop authority.
- Compromising or imitating the MCP connector yields only the narrow authority
  the provider already had to propose a bounded tool call.
- Event and snapshot confidentiality follows task/session scope.
- User decisions become meaningfully reviewable and cryptographically bound to
  execution rather than relying on a generic prompt.
- AI-generated UI quality is judged from the real isolated render with
  reproducible evidence instead of the generating model's confidence.
- New control requests cannot accidentally become available to every client.
- Revocation and uncertain execution have one explicit lifecycle model.

### Costs

- The daemon owns more listener, capability, event-filter, and revocation state.
- MCP execution moves from a sequence of general control calls to a dedicated
  broker state machine.
- Desktop and Agent protocol versions must migrate together.
- Approval UI becomes more detailed and needs careful bounded disclosure and
  accessibility design.
- Multi-viewport visual captures and optional advisory review increase build
  time, artifact storage, baseline maintenance, and false-positive triage.
- Real provider and adversarial sandbox tests add macOS release time and require
  stable fixtures.

### Compatibility

- Existing unauthenticated clients cannot retain full control access after the
  migration.
- Developer CLI access needs an explicit administrative role and private
  credential rather than implicit same-user trust.
- Provider-visible MCP configuration remains compatible with stdio MCP, but its
  connector points at a dedicated capability endpoint.
- Ordinary Terminal WebSocket framing remains separate and does not become part
  of the Agent capability protocol.

## Rejected alternatives

### Only change the socket to mode `0600`

This blocks other operating-system users but not the contained provider, which
runs as the same user and is explicitly granted access to the socket path.

### Check only the Unix peer UID

The trusted desktop, provider, connector, and unrelated user processes normally
share one UID. Peer UID is useful defense in depth but cannot express product
roles.

### Add a client-supplied role to `Hello`

An untrusted caller can choose `DesktopController`. Roles must be assigned by
the server endpoint and launch record.

### Add one secret token to the unrestricted control socket

The provider can inspect the configuration used to launch its MCP child. A
stolen token would still grant the complete request vocabulary. Capability
scope must remain safe even when the provider learns the connector credential.

### Trust the digest-pinned MCP connector

The provider does not need to modify the connector; it can speak the same local
protocol itself. Executable identity does not authenticate process ancestry or
intent.

### Keep one socket and rely on per-handler state checks

State and revision checks validate a target operation, not whether the caller
may invoke that class of operation. A newly added handler could silently widen
every client's authority.

### Let the connector begin and complete operations after approval

That leaves unnecessary state-transition and receipt authority in the
provider-visible process tree. The daemon can own the transition after one
bounded proposal.

### Hide complete details and approve only a digest

A digest binds bytes but does not let a human understand them. Approval needs
both cryptographic binding and a trusted, reviewable representation.

### Let the WebView own approval or capability credentials

Web content and generated UI are replaceable projections. Giving them decision
credentials would reverse the Rust/Native authority boundary.

### Let the generating Agent judge its own UI

The model can explain intent and propose a revision, but its textual
self-assessment is not evidence of the real packaged render. It also creates an
incentive to suppress defects in order to finish the task.

### Treat successful compilation or one screenshot as an aesthetic gate

Compilation proves syntax and module integrity. One happy-path desktop capture
misses responsive, locale, state, focus, theme, and runtime failures.

### Reduce visual quality to one beauty score

A scalar score is difficult to reproduce, hides actionable evidence, embeds
unstated taste, and encourages threshold gaming. The report keeps objective
findings separate from advisory aesthetic judgment.

### Upload every generated UI to a remote vision model

Generated previews may contain private prompts, source-derived text, workspace
identity, or user data. Remote review is an explicit provider operation, never
the default visual gate.

## Follow-up decisions

This ADR intentionally does not fully specify:

- journal compaction and long-term retention policy;
- canonical VT screen-state snapshots for Terminal recovery;
- Tier 2 image acquisition and update UX;
- remote MCP OAuth and per-request credential materialization;
- platform signing, notarization, and update-channel trust;
- complete PTY descendant supervision and exited-session eviction.

The exact optional aesthetic-review model, style rubric, and organization-level
brand policy remain configurable follow-ups. Their outputs stay advisory unless
the user or organization explicitly adopts a review policy; they never become
machine authority.

Those require separate lifecycle or product decisions. They must not delay
removal of ambient Agent access to the general daemon control plane.
