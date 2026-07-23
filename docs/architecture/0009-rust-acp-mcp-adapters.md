# ADR 0009: Embed ACP and MCP adapters behind the Rust permission broker

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md),
  [ADR 0008](0008-license-scoped-warp-reuse.md)

## Context

Hyper Term needs first-class agent sessions and tools without nesting every
agent in a terminal and reconstructing its state from ANSI. ACP and MCP solve
different parts of this problem:

- ACP connects an editor/client to an agent and describes sessions, prompts,
  updates, permissions, plans, and terminal/tool interactions.
- MCP connects a host or agent to tools, resources, prompts, and related
  capabilities over negotiated transports.

External protocol schemas and implementations change independently of the
product. Neither protocol should become the durable task model or gain direct
machine authority.

## Decision

Create renderer-independent Rust adapters based on the official permissive
SDKs:

- `hyper-term-acp` uses `agent-client-protocol` as an ACP client and supervises
  local agent subprocesses over stdio initially;
- `hyper-term-mcp` makes Hyper Term the MCP Host and runs an internal MCP Client,
  using the official `rmcp` SDK with stdio first. Streamable HTTP and OAuth are
  added behind explicit capability profiles; exposing an MCP Server is a future,
  separately reviewed role;
- both adapters live behind `hyper-term-core` ports and have no Tauri or React
  dependency.

The adapters negotiate wire protocol versions and optional capabilities at
connection time. SDK crate versions are recorded separately from negotiated
wire versions. External events map into Hyper Term's versioned internal events;
the raw wire format is bounded diagnostic evidence, not the persistence schema.

```text
ACP agent process ──> AgentDriver ──┐
                                    ├─> domain events -> journal -> projections
MCP server/process ─> ToolDriver ───┘
                          ▲
                          │ authorized ToolOperation
                Rust PermissionBroker
```

## Authority and execution flow

Agent and tool messages may propose effects but do not execute them merely by
arriving:

```text
external request
  -> normalize target, inputs, actor, and capability request
  -> create immutable Operation revision
  -> policy and risk evaluation
  -> preview or exact approval when required
  -> acquire workspace/input/tool lease
  -> dispatch one bounded or explicitly opaque effect through a Rust-owned driver
  -> capture result and verification evidence
  -> emit domain event and external protocol response
```

Deno may transform schemas or render tool output, but it cannot launch ACP or
MCP processes, hold their credentials, or dispatch their effects. Generated UI
can only create an operation proposal under ADR 0004.

Computer Use and terminal execution remain sibling drivers. An MCP server may
offer a Computer or shell-like tool, but its declared name does not grant those
capabilities. The Rust profile evaluates the concrete resource and action.

The broker is authoritative over whether Hyper Term dispatches an operation and
which local capability profile it receives. It cannot decompose or intercept
every syscall inside an ACP agent, an MCP server, or a remote tool. Local
third-party processes therefore require an OS sandbox, isolated worktree,
minimal filesystem mounts, sanitized environment, and capability proxy where
available. A remote or internally opaque tool call is recorded as one opaque
effect with declared scope; loss of its response becomes `UnknownExecution`,
not a claim that no effect occurred.

## Driver lifecycle

Each connection has a manifest and supervised state:

```text
DriverManifest
  driver_id / kind / implementation_version
  negotiated_protocol_version / capabilities
  transport / executable_digest / permission_profile

DriverState
  Starting -> Ready -> Busy -> Waiting -> Closing -> Closed
                                  \-> Failed / UnknownExecution
```

- Rust owns executable resolution, cwd, environment allowlist, credentials,
  stderr capture, cancellation, timeouts, process groups, and restart policy.
- A disconnect during an effect never triggers silent replay. The operation
  becomes `UnknownExecution` until observation or the user resolves it.
- MCP discovery results are scoped to server identity, protocol version, and
  capability revision and have bounded cache lifetime.
- Raw request/response logging is off by default. Metadata and content digests
  are retained; payload capture requires explicit redaction and retention.
- Provider-specific fields survive as bounded diagnostic attachments until an
  adapter understands them, but cannot mutate canonical task state.

## PTY and OSC fallback

An arbitrary CLI remains usable through the PTY. Shell integration, OSC 7,
OSC 133/633, and agent-specific OSC 777/9 can improve presentation, but any
child process can forge them. They never establish actor identity, approval,
permission, or completion.

When both structured ACP and PTY views exist, ACP is the semantic control path
and PTY bytes are terminal evidence. Sequence and correlation IDs link the two
without parsing ANSI to reconstruct protocol state.

## Consequences

Hyper Term gets native Rust protocol types, lifecycle control, and consistent
policy without importing Warp's AGPL MCP implementation. Adapter and SDK churn
is isolated from the journal and UI. This requires translation fixtures and
means some provider-specific features remain unavailable until explicitly
modeled.

The official [ACP Rust library](https://agentclientprotocol.com/libraries/rust)
implements both protocol sides and negotiates protocol/capability versions. The
official [MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) provides
client/server roles and multiple transports; only the minimal reviewed feature
set is enabled per release.

## Implementation status (2026-07-22)

- `agent-client-protocol` 1.2.0 provides the bounded ACP v1 JSONL types and
  framing used by the Rust driver.
- `StructuredAgentClient` is the provider-neutral boundary shared by direct
  Codex app-server and ACP sessions. Provider wire values are projected into
  canonical driver events and operation proposals; they do not become
  `BlockDocument` renderer authority.
- Direct Codex capability discovery now reads a bounded `model/list` and a
  workspace-scoped `skills/list` through the supervised Rust app-server
  adapter. The selected model and supported reasoning effort are validated
  against that catalog and applied to the next `turn/start`; they are not
  presentation-only state. Enabled Skills become `$skill` mentions in the
  composer, while ACP `available_commands_update` entries remain provider
  commands such as `/skills`. Unsupported discovery remains additive: an older
  app-server can still start a thread with an empty capability projection.
- ACP session modes are first-class composer controls. Rust projects the
  bounded `modes` returned by `session/new` ahead of ordinary configuration,
  routes a Native selection through the real `session/set_mode` request, and
  applies later `current_mode_update` notifications back to the same control.
  Ask/Architect/Code labels are provider data; the renderer cannot invent a
  mode or change one without the ACP adapter validating the advertised ID.
- ACP session metadata and context usage are also authenticated session state.
  Rust applies bounded `session_info_update` partial updates and validates
  `usage_update` against a non-empty context window. Native uses the resulting
  title only for the owning Agent tab and shows real usage as a compact
  composer badge; control characters, oversized titles, and impossible usage
  windows fail closed. Optional ACP cost is intentionally not projected until
  currency and precision semantics have a dedicated provider-neutral model.
- The ignored installed-Codex integration gate now covers the authenticated,
  isolated `initialize -> model/list -> skills/list -> thread/start` path and
  requires non-empty model and reasoning choices. Native projection tests prove
  those choices and `$skill` mentions reach the compact composer without giving
  the renderer protocol or filesystem authority.
- Fixtures cover initialization, session creation, prompt streaming, session
  updates, permission proposals, and MCP capability negotiation.
- The desktop supervisor clears the adapter environment, verifies executable
  digests, and delegates authentication/version readiness to one Rust-owned
  probe implementation with a two-second process-group timeout and 4 KiB
  output cap. The same implementation produces the startup inventory, serves
  authenticated `POST /agent/providers` refreshes, and gates creation of each
  known provider session; startup and runtime status therefore cannot drift.
- Native tabs are bound to their selected provider. Direct Codex app-server,
  Codex ACP, Claude ACP, and `copilot --acp --stdio` are separate provider
  registrations chosen explicitly from the Agent menu rather than inferred
  from terminal contents. Disabled menu entries retain the reason they cannot
  start. A login-required Codex or Claude registration also exposes an explicit
  Terminal sign-in action. Native creates an ordinary Terminal session and
  writes only the fixed `codex login` or `claude auth login` text to the system
  clipboard; it does not inject PTY bytes, press Return, probe credentials, or
  launch a provider. The visible guide asks the user to paste and review the
  command, then refreshes the Rust-owned readiness probe. A successful refresh
  clears the guide only when that provider family becomes ready, while a failed
  refresh preserves the last complete status projection. The compile-only
  Agent Block vocabulary is a separate Native fragment so product layout and
  protocol-contract growth remain independent.
- ACP resolution is explicit path first, then the digest-inventoried bundled
  runtime, then a recognized installed package. This keeps automatic startup
  on the adapter version tested with the desktop build while preserving exact
  user overrides and source-checkout fallbacks. Codex accepts both
  `@zed-industries/codex-acp` and `@agentclientprotocol/codex-acp`; Claude
  accepts `@agentclientprotocol/claude-agent-acp`. Automatic discovery
  canonicalizes the executable and requires its bounded, non-symlinked
  `package.json`, scoped `node_modules` location, semantic version, and declared
  `bin` entry to agree. A same-named executable or forged `--version` output is
  not enough. Explicit paths remain available for reviewed standalone builds.
- The signed package retains `@agentclientprotocol/codex-acp` as the offline
  Codex fallback, so a machine without an installed adapter still works when
  the matching Codex CLI is authenticated. The selected executable digest and
  package/version identity are recorded in the driver manifest.
- A 2026-07-23 real-provider probe kept the bundled Codex adapter as the
  automatic default. Both installed Zed ACP 0.15.0 and a clean Zed ACP 0.16.0
  platform binary completed ACP initialization, but their embedded Codex core
  rejected the current `gpt-5.6-sol` model metadata and `max` reasoning value
  during a harmless authenticated prompt. Explicit Zed paths remain supported
  for compatibility testing, but successful initialization alone is not a
  sufficient default-selection gate.
- Ignored, credential-using integration gates completed a harmless real prompt
  through official Codex ACP 1.1.4 and Claude Agent ACP 0.59.0 artifacts. Claude
  subscription authentication on macOS requires the exact Claude executable
  plus `USER` and `LOGNAME` so its Keychain entry remains visible; API keys are
  not inherited by the adapter.
- ACP command catalogs are optional composer metadata, not turn-critical state.
  A real Codex ACP prompt can advertise more than 96 entries when installed
  Skills are projected as commands. Rust now prioritizes `/skills` and `$skill`
  entries, removes duplicates and invalid oversized values, retains at most 96,
  and emits an `available_commands_truncated` protocol notice instead of
  failing the turn. Credential-using gates prove real Codex, Claude, and
  `copilot --acp --stdio` prompts all complete; the Codex gate additionally
  requires the bounded projection to retain `skills`.
- The signed app build now materializes both official adapter packages through
  a frozen Deno lockfile, removes their redundant provider binaries, records a
  bounded per-file digest inventory, and runs the offline entrypoints with the
  already bundled Deno runtime. It follows Deno's production package links but
  omits the top-level `.pnpm` installer store instead of copying the same
  packages and provider binaries twice. Build and verification share the Rust
  desktop loader's 8,192-file and 128-MiB limits. The current arm64 probe is
  5,972 ACP files; both offline version probes pass and the complete ad-hoc app
  is 184 MiB instead of 1.9 GiB. Real prompt gates pass through this exact
  pruned artifact for both providers.
- Release validation now exercises both provider boundaries without credentials
  or network access. The exact packaged Deno runtime loads the frozen Codex ACP
  entrypoint and completes its translated app-server `initialize`; it also
  loads the frozen Claude ACP entrypoint, creates a real ACP session, launches
  the configured external Claude executable, and exchanges the official Claude
  Agent SDK stream-JSON initialization and context-usage frames. The external
  provider fixtures are deterministic, while the adapters and Rust client are
  the production artifacts shipped in the app.
- ACP v1 `session/new` and direct Codex sessions now receive the same
  digest-pinned `hyper-term-mcp` stdio server. Its enabled catalog is derived
  from the exact runtime configuration: Diff is always bounded and read-only;
  GenUI uses the signed Deno/esbuild assets; Deno LSP uses a Rust-created
  private workspace snapshot. Those runtime paths stay in the outer Rust
  daemon, not in the connector's arguments or Agent Seatbelt. The connector
  proxies the authorized, digest-bound invocation over the control socket, so
  Rust can apply the narrower Deno Seatbelt without attempting a forbidden
  nested sandbox. A real-Deno integration test proves a contained ACP Agent can
  discover all three tools, propose and authorize `hyper_term.lsp.query`, and
  receive the real LSP result through the Agent turn.
- Codex ACP emits a transport-level MCP consent request before forwarding the
  actual `tools/call`. Rust correlates that request with the preceding ACP
  `tool_call` by session and tool-call ID, requires the `hyper_term` server,
  exact title, structured input, MCP marker, and allowlisted tool name, and
  answers only `allow_once`. This does not authorize execution: the
  digest-pinned `hyper-term-mcp` process independently validates the exact
  arguments and creates the user-visible broker operation. Missing or
  inconsistent correlation remains a normal fail-closed ACP effect proposal.
- A directory that cannot be captured within the private snapshot limits no
  longer prevents the ACP session itself from starting. Rust omits only the
  Deno LSP capability for that session, deletes the partial snapshot, and keeps
  the stricter Diff and GenUI MCP catalog. This is important for the normal
  Terminal default of starting in the user's home directory: Home is not
  silently treated as one giant Agent workspace.
- Distribution still needs the containment decision in ADR 0014 and the full
  fuzz and protocol-upgrade matrix. Provider sign-in recovery now has a
  terminal-first Native baseline, but packaged credential and Keychain failure
  cases still require release automation. The internal MCP adapter remains a
  bounded stdio subset; adopting the complete Rust MCP SDK remains a release
  gate.

## Validation gates

- Golden fixtures cover initialization, capability negotiation, streaming,
  cancellation, permission requests, reconnect, malformed frames, stderr, and
  unknown messages for each supported protocol version.
- Arbitrary stream chunking and backpressure must not alter event order or
  reducer output.
- Kill an agent or MCP server before, during, and after an effect and verify the
  correct `Failed` versus `UnknownExecution` state with no silent replay.
- Verify an agent, MCP server, Deno sidecar, preview, and PTY marker cannot bypass
  the same Rust permission policy.
- Fuzz framing and schema adapters; cap frame, payload, queue, discovery, and
  diagnostic sizes.
- Test redaction with credentials, private files, terminal output, and tool
  payloads before allowing optional raw capture.
- Pin each ACP and MCP SDK version, enabled features, lockfile digest, and MSRV;
  review them independently from negotiated wire protocol versions.
- Prove protocol SDK upgrades do not change canonical event fixtures without an
  explicit schema migration.
