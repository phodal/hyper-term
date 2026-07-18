# ADR 0002: Separate the Rust control kernel, Deno tool runtime, and UI projections

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0001](0001-hybrid-renderer-spike.md)

## Context

Hyper Term is moving from a terminal spike to an agentic workbench that can
generate and edit React interfaces, render live previews, connect agents and
tools, and preserve enough history to debug a failed interaction locally.

These features introduce three failure-prone systems next to the terminal:

- a JavaScript/TypeScript runtime and package toolchain;
- a WebView UI that may be reloaded or replaced;
- AI-generated UI code that must be treated as untrusted.

None of them may become the authority for PTYs, filesystem changes, agent tool
calls, Computer Use, approvals, or durable history. ADR 0001 already separates
the Rust PTY core from Tauri and treats xterm.js as a replaceable renderer. This
ADR extends that rule to the whole agentic product.

## Decision

Hyper Term will use four explicit security and lifecycle domains:

```text
┌──────────────── trusted desktop client ────────────────┐
│ React workbench       native terminal projection       │
│ editor adapters       diff/review/attention            │
└───────────────┬───────────────────────┬─────────────────┘
                │ typed intents         │ snapshot/delta
┌───────────────▼───────────────────────▼─────────────────┐
│ hyperd / hyper-term-core — Rust control kernel          │
│ PTY + processes     operation journal     permissions   │
│ ACP/MCP drivers     artifact store         input lease   │
└──────────┬──────────────────────┬────────────────────────┘
           │ supervised IPC       │ validated artifact
┌──────────▼─────────────┐  ┌─────▼────────────────────────┐
│ bundled Deno runtime  │  │ isolated Agentic UI Preview  │
│ TS tools, LSP, graph  │  │ React artifact, no native API│
└────────────────────────┘  └──────────────────────────────┘
```

The Rust control kernel is authoritative for granting, scoping, accepting, and
recording:

- PTYs, subprocess groups, terminal sessions, and input leases;
- workspace and durable file revisions, worktrees, credentials, network grants,
  and Computer Use;
- agent and MCP process lifetimes;
- approvals, operation revisions, event ordering, snapshots, and retention;
- accepted UI artifacts and the capability tokens they may reference.

An authorized external agent or MCP server may still perform opaque effects
inside its process. Local drivers therefore run with an isolated worktree,
minimal mounts, OS sandbox, or capability proxy where available. Rust controls
whether and with what profile an effect is dispatched; it does not claim to
intercept every syscall made by local or remote third-party code.

The Deno runtime is a supervised development-tool service. It may analyze
immutable virtual source snapshots, use a Rust-allocated private content cache
and scratch directory, resolve a locked dependency graph, provide TypeScript
tooling, and perform pure transformations. It receives no ambient workspace
write authority and cannot launch shells, agents, or MCP servers.

The trusted React workbench renders versioned Rust-owned state and submits
intents. It does not become the durable domain model. AI-generated React runs in
a separate preview realm with no Tauri commands, Deno API, filesystem API, or
raw Rust bridge.

## Data-plane rules

Each high-volume stream is independent from React component state:

- PTY bytes use ordered, bounded chunks and a terminal-specific projection;
- model tokens and agent events are reduced and emitted at a frame-sized cadence;
- editor transactions are batched by document revision;
- UI projections carry sequence numbers, acknowledgements, and snapshot recovery;
- a slow or restarted client resumes from a snapshot plus ordered deltas.

Terminal bytes, model output, web content, generated source, and UI trace events
are untrusted inputs. None can be interpreted as an application command.

The native terminal renderer bake-off from ADR 0001 remains valid. React may
host editor, agent, diff, review, and artifact surfaces, but it does not make the
PTY hot path a React responsibility.

## Consequences

Positive consequences:

- a WebView, Deno, generated UI, or editor crash cannot kill terminal sessions;
- Rust policy is shared by desktop, daemon, test, and future remote clients;
- JavaScript tooling can evolve independently of the durable event schema;
- preview isolation makes generated UI useful without granting it native power.

Costs and constraints:

- there are explicit IPC, snapshot, and schema-version boundaries to maintain;
- the application ships and supervises more than one process;
- UI features that need native effects must use a proposal/approval round trip;
- low latency depends on batching and warm workers rather than shared memory.

## Rejected alternatives

- **Put PTY and agent state in the React store.** A WebView reload would become a
  session and history failure.
- **Expose a generic Tauri `invoke`, filesystem, or command bridge.** Generated
  or compromised UI could bypass operation revisions and permission policy.
- **Make Deno the application host.** It would reverse the ownership boundary
  and tie native authority to the JS runtime lifecycle.
- **Render all terminal output through React.** It couples burst throughput and
  correctness to a general-purpose component reconciliation loop.

## Validation gates

- Killing the WebView or Deno process must not stop a PTY or lose an approval.
- A restarted client must reconstruct the same task and artifact state from a
  snapshot plus events.
- A preview realm must be unable to call Tauri, Deno, shell, filesystem, or
  network primitives except through its declared host protocol.
- Burst and reconnect tests must prove sequence continuity, bounded memory, and
  snapshot recovery for terminal, agent, and editor streams.
- Every native effect must be attributable to an immutable operation revision.
