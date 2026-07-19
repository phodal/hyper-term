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

## Implementation status (2026-07-19)

- `agent-client-protocol` 1.2.0 provides the bounded ACP v1 JSONL types and
  framing used by the Rust driver.
- `StructuredAgentClient` is the provider-neutral boundary shared by direct
  Codex app-server and ACP sessions. Provider wire values are projected into
  canonical driver events and operation proposals; they do not become
  `BlockDocument` renderer authority.
- Fixtures cover initialization, session creation, prompt streaming, session
  updates, permission proposals, and MCP capability negotiation.
- The desktop supervisor clears the adapter environment, verifies executable
  digests, only auto-discovers adapters with the official version signature,
  and exposes a bounded provider inventory to the Native host.
- Native tabs are bound to their selected provider. Codex and Claude are chosen
  explicitly from the Agent menu rather than inferred from terminal contents.
- Ignored, credential-using integration gates completed a harmless real prompt
  through official Codex ACP 1.1.4 and Claude Agent ACP 0.59.0 artifacts. Claude
  subscription authentication on macOS requires the exact Claude executable
  plus `USER` and `LOGNAME` so its Keychain entry remains visible; API keys are
  not inherited by the adapter.
- The signed app build now materializes both official adapter packages through
  a frozen Deno lockfile, removes their redundant provider binaries, records a
  bounded per-file digest inventory, and runs the offline entrypoints with the
  already bundled Deno runtime. Real prompt gates pass through this exact pruned
  artifact for both providers.
- Distribution still needs the containment decision in ADR 0014, a
  terminal-auth UI, and the full fuzz and protocol-upgrade matrix. The internal
  MCP adapter remains a bounded stdio subset; adopting the complete Rust MCP SDK
  remains a release gate.

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
