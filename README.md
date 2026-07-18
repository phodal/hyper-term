# Hyper Term

Hyper Term is a design-stage project for a local-first, agentic terminal. It is
not a terminal with a chat sidebar. It treats Shell, structured agents,
Computer Use, MCP tools, editors, browsers, and generated interfaces as
executors and projections of one durable human–AI task model.

> **Repository status:** M1 implementation and the first M3 risk spike are in
> progress. The disposable Vite/Tauri prototype has been removed. The
> repository now contains a renderer-independent Rust protocol, journal,
> operation reducer, Block projector, reconnectable PTY daemon, and a
> terminal-first React Workbench built directly by pinned Deno. Proposed ADRs
> remain subject to the validation and replacement gates recorded in each
> decision.

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
| **M1 — Durable Rust kernel (current)** | Build renderer-independent `hyper-term-core` and an out-of-process `hyperd` with PTY supervision, append-only journal, checkpoints, permission broker, input lease, bounded transcript, and reconnectable ordered streams. | A direct user terminal resolves the configured login shell without renderer-supplied executables; zsh login/interactive mode, UTF-8, truecolor, foreground-job `Ctrl-C`, resize/output ordering, reconnect, cancellation, and recovery have tests and release probes. |
| **M2 — Agent control loop and Block workbench** | Add raw PTY agent compatibility, one ACP v1 adapter, an MCP host behind the broker, `BlockProjector`, attention reducer, and renderer-independent Workbench assets built by Deno. | One task reaches `ReviewReady` through proposal, approval, execution, verification, and review; all ACP v1 variants have golden fixtures; renderer failure loses no canonical state. |
| **M3 — Agentic UI and local debugging (risk spike active)** | Add the brokered Deno tool runtime, persistent `esbuild-wasm` compilation, versioned UI IR/React artifacts, trusted editor adapters, isolated previews, source maps, and semantic Time Travel. | A broken generated UI keeps its last-known-good artifact, maps errors to its source revision, and replays without Shell, network, MCP, or Computer Use effects. |
| **M4 — Computer Use, voice, and attention** | Implement observe–act–verify drivers, explicit capability and focus leases, before/after evidence, voice briefs, push-to-talk steering, local pause/takeover controls, and semantic notifications. | Stale observations and lease conflicts are rejected; every action has actor, target, capability, receipt, and result; voice never directly approves a consequential effect. |
| **M5 — Native desktop product shell** | Use Native SDK as the default macOS host for the ordinary terminal surface and common blocks, with bounded Web/WASM islands for generated UI and previews. | Startup, key-to-present, burst throughput, 100k-block virtualization, CJK/IME/accessibility, responsive layout, crash recovery, focus, and a shared Native/Web design-token contract pass on the packaged app. |
| **M6 — Distribution and ecosystem** | Add signed/updatable desktop packages, SSH/remote sessions, provider adapters, extension manifests, policy profiles, import/export, diagnostics, and a stable compatibility contract. | Reproducible builds, migration/rollback, supply-chain verification, least-privilege defaults, recovery drills, and supported-platform matrices are release-ready. |

Native SDK is the default desktop host target. Renderer work must still preserve
the durable task model, permission boundary, structured agent loop, and ordinary
PTY path instead of moving machine authority into UI code.

## Traditional Terminal contract

Hyper Term must first be a fast, ordinary terminal. Creating a default terminal
is an explicit human action, so it opens the authority-selected user login shell
without creating an AI operation. The client may choose only an absolute working
directory and terminal size; it cannot supply a program, arguments, or
environment. Rust resolves and validates the shell, owns the controlling PTY,
sets `TERM=xterm-256color` and truecolor metadata, orders output before exit,
handles input leases and resize generations, and keeps replay available after a
client reconnects.

Agent, Block, transcript, and evidence consumers subscribe outside the PTY hot
path. They may fall behind or request a snapshot; they may not delay keyboard
input, shell echo, or native presentation. The first machine-readable release
baseline is recorded in
[Terminal core release baseline](docs/benchmarks/terminal-core-baseline-2026-07-18.md).

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
crates/hyper-term-daemon/    permissioned, reconnectable control-kernel host
crates/hyper-term-drivers/   bounded sidecar supervision and Deno LSP client
apps/workbench/         terminal-first React Block workbench and isolated GenUI
runtime/                pinned supervised-runtime manifests
scripts/                Deno build and supply-chain verification tools
docs/architecture/     numbered architecture decisions
docs/research/         dated product and implementation evidence
```

Implementation lands in milestone-sized, independently tested slices. Every
protocol or lifecycle change must include tests, and every milestone must
preserve the Rust authority and renderer-independence boundaries.

## Develop the Rust kernel

The current M1 slice requires the pinned Rust toolchain only:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p hyper-term-daemon --bin hyperd -- \
  --state-dir .hyper-term \
  --socket .hyper-term/hyperd.sock
```

`hyperd` exposes a versioned Unix-socket protocol. Control messages are bounded
JSON frames; PTY input, output, and snapshots use separate bounded binary
frames. A client must complete `Hello`, receive exact-operation authorization,
and acquire the terminal input lease before it can write to a PTY.

## Develop the Deno Workbench

The Workbench is a static browser artifact. It has no Vite dev server, Node.js
runtime, or renderer-side machine authority. Deno resolves the frozen lockfile
and bundles React, CodeMirror, the compiler Worker, and the isolated preview
shell. Generated UI source is compiled by a persistent `esbuild-wasm` Worker
against an explicit, size-bounded virtual filesystem.

Use Deno `2.9.3`, pinned in `runtime/deno-manifest.json`:

```bash
deno task verify:runtime
deno task check
deno task test
deno task build:workbench
python3 -m http.server 4173 --bind 127.0.0.1 --directory dist/workbench
```

The preview iframe keeps an opaque sandbox origin. Its trusted runtime capsule
is inlined at build time, accepts only channel-bound accepted artifacts, checks
their SHA-256 digest again, and denies network access. A failed compile updates
diagnostics and Time Travel history without replacing the last-known-good
artifact.

The Rust `hyper-term-drivers` crate launches the same pinned Deno executable
with a cleared environment, dedicated cache and scratch roots, bounded LSP
framing, bounded stderr capture, and process-group shutdown. Its ignored
integration test performs a real `initialize`, TypeScript diagnostics, and
`shutdown` exchange when the verified runtime path and executable digest are
provided:

```bash
HYPER_TERM_DENO_PATH=/absolute/path/to/deno \
HYPER_TERM_DENO_SHA256=<manifest-executable-sha256> \
cargo test -p hyper-term-drivers --test deno_lsp -- --ignored
```

## Release the macOS application

Pushing a version tag such as `v0.1.0`, or manually dispatching the
`Release Hyper Term` workflow for an existing tag, validates the Rust, Deno, and
Native SDK layers and builds complete Apple Silicon and Intel applications. The
release archives contain the Rust desktop supervisor, Native SDK renderer, and
terminal WebView assets in one `.app` bundle. Stable releases are Developer ID
signed and notarized; unsigned RC pipeline tests are labelled explicitly.

The stable part of the tag must match the Cargo workspace and
`apps/desktop/app.zon` versions. Configure the protected `Release` GitHub
environment with the Apple signing and App Store Connect API secrets before
creating a tag. See
[the macOS release guide](docs/release/macos-app.md) for the bundle layout and
required secrets.

## Structured agent adapters

ACP, Codex app-server, Claude stream-json, and opaque PTY agents are separate
transports behind one internal `AgentDriverEvent` boundary. They are not treated
as interchangeable wire protocols. The current Codex adapter launches an exact
binary digest with a cleared environment, negotiates app-server v2 over bounded
JSONL, and turns command/file approval requests into inert
`AgentEffectProposal` values. Only a matching, revisioned Rust operation
authorization can produce the external approval response; persistent policy
choices are not forwarded as one-turn wire approvals.

The real installed-binary handshake is available as an ignored integration
test. It uses an isolated `CODEX_HOME`, so it does not read the user's normal
Codex profile or perform a model turn:

```bash
HYPER_TERM_CODEX_PATH=/absolute/path/to/codex \
HYPER_TERM_CODEX_SHA256=<inspected-executable-sha256> \
cargo test -p hyper-term-drivers --test codex_app_server -- --ignored
```

## License

Apache-2.0. Competitor implementations are research input only; do not copy
GPL/AGPL code into this repository without a separate, explicit licensing
decision.
