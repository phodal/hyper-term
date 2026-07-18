# ADR 0008: Reuse Warp by license and boundary, not by embedding its core

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md)

## Context

The local `/Users/phodal/ai/warp` checkout is a valuable, current Rust terminal
and agent implementation. It contains terminal modeling, PTY event loops, MCP,
Computer Use, voice input, persistence, permissions, IPC, and a managed Node
runtime. It is tempting to make these crates direct Hyper Term dependencies.

The audit used Git commit
[`0017f305`](https://github.com/warpdotdev/warp/tree/0017f3059a4ca705c2b716f2c44ab9761b24c2b0)
because the local Warp index and working tree were in an unrelated, inconsistent
state. No Warp files were restored or modified.

Warp's [licensing section](https://github.com/warpdotdev/warp/blob/0017f3059a4ca705c2b716f2c44ab9761b24c2b0/README.md#licensing)
states that only `warpui_core` and `warpui` are MIT licensed. The rest of the
repository, including the capabilities considered here, is AGPL-3.0-only. Hyper
Term currently declares Apache-2.0.

## Decision

Hyper Term will not copy, vendor, link, or path-depend on Warp's AGPL terminal,
MCP, Computer Use, voice, persistence, permission, IPC, or application crates
while Hyper Term remains Apache-2.0.

Warp is used as a research and behavior corpus. Hyper Term defines its own
traits and protocols first, then implements them from permissively licensed
upstream crates and public specifications with an explicit provenance record.
Any direct dependency requires a per-crate license allowlist and dependency
review. A separate process integration with an installed Warp product may be
evaluated through a documented public protocol, but it does not authorize code
copying or linking.

Changing Hyper Term to AGPL, obtaining a different license from Warp, or a Warp
license change requires an explicit replacement ADR; it is not inferred from a
local checkout.

## Audit findings and disposition

| Capability | Warp snapshot | Hyper Term disposition |
| --- | --- | --- |
| ACP | ACP is still roadmap work. Current CLI-agent status relies on PTY observation and OSC 777/9. | Use the official ACP Rust SDK; PTY/OSC is an untrusted compatibility fallback. |
| MCP | `crates/mcp` builds on `rmcp` and supports multiple transports, OAuth, and discovery; application-layer code adds reconnect behavior. The stack couples to Warp core, UI, and cloud models. | Build a small adapter directly on the official `rmcp` SDK. |
| PTY/terminal | `warp_terminal` is a terminal model; actual spawn and event-loop ownership live deeply in the application crate. | Retain the independent `hyper-term-core` PTY boundary and evaluate permissive terminal models. |
| Computer Use | The crate has valuable cross-platform target/action patterns but is AGPL. | Independently implement a capability-scoped Computer driver from OS APIs and permissive crates. |
| Voice | The crate mainly composes `cpal`, `rubato`, and WAV encoding; it does not provide the full VAD/ASR/intent experience. | Use the upstream audio crates directly behind the input adapter in ADR 0007. |
| Persistence | Diesel/SQLite uses WAL, a bounded single writer, and materialized product state; it is not a general event-sourced ledger. | Adopt the single-writer lesson but design the ADR 0006 journal around Hyper Term events. |
| Permissions | Typed decisions and command analysis are useful, but policy is coupled to Warp product state and some model risk labels. | Rust capability policy is authoritative and never trusts a model's risk label. |
| Node runtime | It downloads and manages a pinned Node distribution; it is not an embeddable JavaScript runtime abstraction. | Implement the Deno distribution and supervisor specified by ADR 0003. |
| IPC | Older IPC paths lack the authentication, framing bounds, and timeouts required for a privileged Deno bridge. | Use owner-only endpoints, framed size limits, action-scoped credentials, version negotiation, and deadlines. |

## Design lessons retained

The following patterns are requirements, not copied implementation:

- keep PTY read, terminal-model reduction, semantic shell events, and UI wakeups
  on separate bounded paths;
- serialize input, resize, exit, and shutdown through a narrow ordered protocol;
- limit bytes processed while holding the terminal-model lock and coalesce wakes;
- assign stable command/block identity with cwd, timing, exit, and actor metadata;
- use a single durable writer and materialized projections for query speed;
- treat OSC agent and command markers as forgeable presentation hints;
- separate fast completion events from slower indexing and enrichment work;
- default audit logs to metadata and content hashes, because raw MCP and agent
  payloads can contain credentials and private content.

## Approved upstream direction

Subject to normal dependency review, the intended sources are:

- `agent-client-protocol` for ACP;
- the official `rmcp` SDK for MCP;
- `portable-pty` plus a separately evaluated terminal model for PTY/VT;
- platform APIs and permissive libraries for Computer Use;
- `cpal`, `rubato`, and a separately selected VAD/ASR layer for voice;
- SQLite with a Hyper Term-owned journal schema and migration layer.

The MIT `warpui_core` and `warpui` crates remain legally eligible for review,
but the React/WebView workbench gives no current architectural reason to add
them.

## Consequences

Hyper Term cannot obtain Warp's mature behavior by dropping its workspace
crates into `Cargo.toml`. This increases implementation time, especially for
Computer Use and terminal semantics, but avoids license ambiguity and importing
a tightly coupled application graph. The audit still reduces design risk by
identifying queueing, persistence, protocol, and redaction failure modes early.

## Validation gates

- Add an automated license/provenance allowlist before introducing reviewed
  third-party source or binaries.
- Every new capability crate records its upstream specification, dependencies,
  license, and implementation provenance.
- No build graph or distributed source/binary artifact may link or bundle a Warp
  AGPL crate while the project is Apache-2.0. SBOMs must report all components
  that are actually present rather than filtering by policy.
- Protocol fixtures and behavior tests must be authored from public contracts,
  not copied Warp test or implementation code.
- Review the product license and distribution model before any decision to fork
  or modify AGPL code.
