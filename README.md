# Hyper Term

Hyper Term is a design-stage project for a local-first, agentic terminal. It is
not a terminal with a chat sidebar. It treats Shell, structured agents,
Computer Use, MCP tools, editors, browsers, and generated interfaces as
executors and projections of one durable human–AI task model.

> **Repository status:** M1 implementation is in progress. The disposable
> Vite/Tauri prototype has been removed; the repository now contains the first
> renderer-independent Rust protocol, journal, operation reducer, Block
> projector, and PTY supervision slice. Proposed ADRs remain subject to the
> validation and replacement gates recorded in each decision.

## Product thesis

The product model changes from:

```text
tab -> pane -> process -> scrollback
```

to:

```text
Intent -> Task -> Operation -> Effect -> Evidence
```

Four product primitives make that useful:

- **Mission Composer:** turns text, voice, files, terminal blocks, screenshots,
  constraints, and acceptance criteria into an editable task contract.
- **Operation Ledger:** records actor, intent, authority, input, effect,
  verification, uncertainty, and recovery state instead of retaining only Bash
  command strings.
- **Attention OS:** models waiting, approval, conflict, failure, takeover,
  review-ready, and completion as durable state rather than notification text.
- **Block Workbench:** projects messages, tools, terminals, diffs, plans,
  approvals, Computer Use evidence, and generated artifacts through native,
  terminal-specific, or WebView renderers without giving those renderers
  machine authority.

## Non-negotiable boundaries

- Rust owns PTYs, process groups, task and operation state, the journal,
  checkpoints, permissions, accepted artifacts, secrets, and agent/tool drivers.
- `hyper-term-core` stays independent from Tauri, Native SDK, React, and any
  other renderer host.
- The WebView owns presentation and interaction only. It receives no generic
  process, filesystem, environment, credential, or socket capability.
- A model or generated UI may propose an action; only the Rust permission broker
  may authorize and dispatch it.
- Terminal output, remote content, ACP metadata, and model text are untrusted
  data. None becomes an application command or renderer registration.
- PTY, model, and Block streams are ordered, bounded, reconnectable channels;
  bulk terminal or artifact data does not travel through a JSON UI bridge.
- Deno is a supervised tooling sidecar and ahead-of-time workbench builder, not
  the control kernel. Interactive Agentic UI compilation uses a persistent,
  bounded `esbuild-wasm` worker.
- ACP and MCP are versioned input adapters. The renderer consumes the internal
  `BlockDocument`, never protocol DTOs directly.

## Target shape

```text
Text / Voice / Context
          |
          v
Mission Composer -----> Task / Run / Operation journal
                              |
                     Rust hyperd control kernel
                    /          |            \
              PTY / SSH      ACP / MCP    Computer Use
                    \          |            /
                     Effect + receipt + evidence
                              |
                         BlockProjector
                              |
                  versioned BlockDocument + patches
                   /             |              \
             Native blocks   Terminal surface   WebView islands
```

Native UI, Tauri/React, a daemon client, and tests must be able to consume the
same Block snapshots and patches and emit the same typed `UiIntent`. A WebView
block is a logical child of the document but may be a separately pooled OS
surface aligned to a native layout slot.

## First vertical slice

The first implementation is intentionally one complete workflow, not a set of
disconnected framework demos:

```text
editable task brief
  -> structured ACP agent session
  -> exact Shell operation proposal
  -> policy and human approval
  -> Rust-owned PTY execution
  -> diff and test evidence
  -> read-only Computer Use verification
  -> ReviewReady bundle
  -> semantic replay with no repeated side effects
```

The compatibility path must simultaneously run an opaque Codex, Claude Code, or
other TUI through a normal PTY. The structured path must not scrape ANSI to
recover plans, tools, approvals, or completion state.

## Roadmap

The roadmap is gate-driven rather than date-driven. A milestone advances only
when its failure, reconnect, security, and evidence gates pass.

| Milestone | Outcome | Exit gates |
| --- | --- | --- |
| **M0 — Architecture baseline** | Review the twelve ADRs, freeze the first `EventEnvelope`, Task/Run/Operation, `BlockDocument`, `UiIntent`, terminal-stream, and artifact schemas. | Proposed ADRs are accepted, revised, or explicitly deferred; golden protocol fixtures and benchmark workloads are specified before implementation. |
| **M1 — Durable Rust kernel (current)** | Build renderer-independent `hyper-term-core` and an out-of-process `hyperd` with PTY supervision, append-only journal, checkpoints, permission broker, input lease, bounded transcript, and reconnectable ordered streams. | Killing and reconnecting a client does not kill the PTY or duplicate an uncertain effect; resize/output ordering, process trees, cancellation, and recovery have tests. |
| **M2 — Agent control loop and Block workbench** | Add raw PTY agent compatibility, one ACP v1 adapter, an MCP host behind the broker, `BlockProjector`, attention reducer, and a minimal Tauri reference client built as static Deno assets. | One task reaches `ReviewReady` through proposal, approval, execution, verification, and review; all ACP v1 variants have golden fixtures; WebView failure loses no canonical state. |
| **M3 — Agentic UI and local debugging** | Add the brokered Deno tool runtime, persistent `esbuild-wasm` compilation, versioned UI IR/React artifacts, trusted editor adapters, isolated previews, source maps, and semantic Time Travel. | A broken generated UI keeps its last-known-good artifact, maps errors to its source revision, and replays without Shell, network, MCP, or Computer Use effects. |
| **M4 — Computer Use, voice, and attention** | Implement observe–act–verify drivers, explicit capability and focus leases, before/after evidence, voice briefs, push-to-talk steering, local pause/takeover controls, and semantic notifications. | Stale observations and lease conflicts are rejected; every action has actor, target, capability, receipt, and result; voice never directly approves a consequential effect. |
| **M5 — Native renderer bake-off** | Compare the Tauri baseline with a macOS-first Native SDK client using the same Block protocol: native common blocks, a terminal surface experiment, and bounded trusted/isolated WebView islands. | Startup, key-to-present, burst throughput, 100k-block virtualization, CJK/IME/accessibility, crash recovery, focus, and cross-platform gates justify either adoption or rejection in a new ADR. |
| **M6 — Distribution and ecosystem** | Add signed/updatable desktop packages, SSH/remote sessions, provider adapters, extension manifests, policy profiles, import/export, diagnostics, and a stable compatibility contract. | Reproducible builds, migration/rollback, supply-chain verification, least-privilege defaults, recovery drills, and supported-platform matrices are release-ready. |

The native renderer is deliberately late. Renderer work must not delay the
durable task model, permission boundary, structured agent loop, or compatibility
PTY path.

## Architecture decisions

### Runtime and authority

- [ADR 0001 — Hybrid renderer spike and native terminal target](docs/architecture/0001-hybrid-renderer-spike.md)
- [ADR 0002 — Rust, Deno, and UI authority boundaries](docs/architecture/0002-runtime-authority-boundaries.md)
- [ADR 0003 — Brokered Deno sidecar](docs/architecture/0003-brokered-deno-sidecar.md)
- [ADR 0008 — License-scoped Warp reuse](docs/architecture/0008-license-scoped-warp-reuse.md)
- [ADR 0009 — Rust ACP and MCP adapters](docs/architecture/0009-rust-acp-mcp-adapters.md)

### Agentic UI and history

- [ADR 0004 — Versioned Agentic UI artifacts](docs/architecture/0004-versioned-agentic-ui-artifacts.md)
- [ADR 0005 — Incremental React compilation with esbuild-wasm](docs/architecture/0005-incremental-react-compilation.md)
- [ADR 0006 — Semantic Time Travel](docs/architecture/0006-semantic-time-travel-debug.md)
- [ADR 0007 — React workbench and transactional editors](docs/architecture/0007-react-workbench-and-editors.md)
- [ADR 0010 — Deno-first static workbench without Vite](docs/architecture/0010-deno-first-static-workbench-build.md)

### Rendering

- [ADR 0011 — Versioned Block Render document](docs/architecture/0011-versioned-block-render-document.md)
- [ADR 0012 — Tauri baseline and Native SDK renderer spike](docs/architecture/0012-native-sdk-renderer-spike.md)

ADR 0001 is accepted for its historical spike. ADRs 0002–0012 remain proposed
until M0 review closes their open validation and replacement gates.

## Research baseline

- [AI Terminal user needs and product thesis](docs/research/ai-terminal-user-needs-2026-07.md)
- [AI CLI and Computer Use probe](docs/research/ai-cli-computer-use-probe-2026-07.md)
- [IntelliJ IDEA New Terminal audit](docs/research/idea-new-terminal-audit-2026-07.md)
- [AI-era terminal landscape](docs/research/terminal-landscape-2026-07.md)

These documents are dated evidence, not permanent product truth. Recheck
versions, protocol schemas, platform support, licenses, and performance claims
before using them for an implementation decision.

## Repository map

```text
README.md              product boundary and roadmap
AGENTS.md              contributor and safety rules
Cargo.toml             Rust workspace definition
crates/hyper-term-protocol/  versioned events, blocks, and wire frames
crates/hyper-term-core/      journal, reducers, projections, and PTY ownership
crates/hyper-term-daemon/    out-of-process control-kernel host (in progress)
docs/architecture/     numbered architecture decisions
docs/research/         dated product and implementation evidence
```

Implementation lands in milestone-sized, independently tested slices. Every
protocol or lifecycle change must include tests, and every milestone must
preserve the Rust authority and renderer-independence boundaries.

## License

Apache-2.0. Competitor implementations are research input only; do not copy
GPL/AGPL code into this repository without a separate, explicit licensing
decision.
