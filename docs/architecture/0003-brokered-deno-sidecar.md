# ADR 0003: Bundle Deno as a brokered sidecar and defer in-process embedding

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md)

## Context

The Agentic UI toolchain needs TypeScript/TSX, npm and JSR packages, language
services, formatting, checking, and a runtime for pure extension logic. Deno is
implemented in Rust and uses V8 and Tokio, but that does not make linking its
internal crates into the terminal process the safest integration.

`deno_core` supplies a V8 `JsRuntime`, an event loop, extensions, and Rust ops.
It does not supply the complete Deno CLI, TypeScript toolchain, npm/JSR loading,
or the CLI permission model. `deno_runtime` adds many `Deno.*` operations but is
a slim, rapidly changing internal layer; the full CLI plumbing is still not a
stable embeddable library. An in-process V8 fatal error, out-of-memory failure,
or dependency upgrade would share the PTY process's fate.

## Decision

Hyper Term will distribute an exact, signed Deno executable as an external
binary and run it as one or more Rust-supervised sidecars.

Each application release records the Deno version, target triple, SHA-256,
signing identity, and tool protocol version in a runtime manifest. The Rust
supervisor verifies this manifest before launch, places the child in an OS
process group or job object, sanitizes its environment, applies resource and
deadline limits, and owns restart and update rollback.

The sidecar communicates over a dedicated local Unix socket or Windows named
pipe using framed, versioned JSON messages. Standard error is a bounded log
stream and is never mixed with protocol frames.

Different trust domains use different processes, not merely different ES
modules or workers. A package-analysis process, a workspace language-service
process, and a future extension process may therefore have different capability
profiles and failure budgets.

## Permission integration

The shipped Deno version must support the external permission broker. Rust sets
`DENO_PERMISSION_BROKER_PATH` to a private Unix socket or named pipe owned by
Hyper Term's `PermissionBroker`.

When this broker is active, Deno delegates checks made through its runtime
permission system to it, ignores CLI allow/deny flags, suppresses interactive
permission prompts, and exits if the broker connection or message ordering
fails. This is a runtime capability decision point, not proof that every Deno
CLI or native behavior was checked, and it is defense in depth rather than an
OS sandbox.

The runtime policy is:

- normal edit and replay sessions are offline. Commands that support the flags,
  such as relevant check, bundle, or install steps, use frozen and cache-only
  modes; LSP and other long-lived modes use a prewarmed cache, denied network,
  selected working directories, and OS containment;
- import maps, `deno.lock`, package cache, and allowed origins are release or
  workspace artifacts with content digests;
- dependency installation is a separate, user-visible operation with network
  capability. Rust validates, accepts, and records the resulting lock/cache
  revision before another runtime may use it;
- `run`, FFI, Node-API native addons, and arbitrary executable access are denied;
- agents and MCP servers are always launched by Rust, never by Deno;
- all broker decisions are linked to the requesting operation and runtime PID.

Initial static module graph imports can load local, npm, JSR, or remote modules
without a Deno runtime permission check. Deno CLI tooling also cannot be assumed
to route every internal read or network action through runtime permissions, and
FFI or native code can bypass the permission layer. Curated import maps, frozen
locks where supported, prewarmed offline caches, selected working directories,
OS containment, and provenance checks therefore remain mandatory.

See Deno's [external permission broker](https://docs.deno.com/runtime/fundamentals/security/#permission-broker),
[permission limitations](https://docs.deno.com/runtime/fundamentals/security/#permissions-that-bypass-the-sandbox),
and [supply-chain guidance](https://docs.deno.com/runtime/packages/supply_chain/).

## Deno responsibilities

Deno may provide:

- `deno lsp`, TypeScript diagnostics, completion, formatting, and checking;
- locked npm/JSR graph resolution and cache preparation;
- pure UI source transforms and compiler validation;
- test and export-time verification of generated React projects;
- pure extension adapters whose inputs and outputs are bounded DTOs.

Rust owns workspace file authority and artifact acceptance. Deno receives
immutable virtual source snapshots or a Rust-created read-only snapshot root,
plus a private content-addressed cache and scratch directory. It never writes
the live workspace directly. It does not own PTYs, shell execution, ACP/MCP
effects, Computer Use, permissions, accepted file/cache/artifact revisions, or
the operation journal.

## Deferred choices

- `deno_core` may later host a small, trusted, snapshot-backed script engine on
  a dedicated thread. It is not the v1 UI toolchain.
- `deno_runtime` remains deferred until it exposes a stable embedding surface
  that materially reduces sidecar cost.
- Deno's experimental bundler may be a compiler backend behind the artifact
  protocol; it is not the artifact protocol itself.
- Deno Desktop is not the application host. It is too new and would make the JS
  runtime own the native window and capability bridge.

## Consequences

The main benefit is failure and upgrade isolation while retaining the complete
Deno toolchain. The costs are application size, an additional resident process,
cross-platform binary signing, cache management, and IPC latency. Long-lived,
lazy sidecars amortize startup cost; no Deno process is started for a terminal-
only session.

## Implementation evidence (2026-07-19)

The first cold-path compiler uses one Rust supervisor per Deno child and bounded
JSON Lines over inherited stdio rather than a multiplexed local socket. It
starts only after the permission broker authorizes
`hyper_term.genui.compile`. Rust verifies the Deno executable, compiler script,
and `esbuild.wasm` digests, clears the environment, grants read access only to
the two runtime assets, waits for a versioned ready message, and recomputes the
returned artifact digest. The packaged macOS app carries and signs all three
runtime files; a signed-bundle probe compiles successfully with network, write,
run, FFI, and workspace access absent.

This closes the basic distribution, framing, and least-Deno-permission spike.
It does not close this ADR's OS-containment, kill/hang recovery, resource-limit,
or cross-platform rollback gates. Those remain milestone gates rather than
being inferred from Deno CLI permissions.

## Validation gates

- Measure cold start, warm request latency, idle CPU, resident memory, and
  installer-size delta on every supported target.
- Kill, hang, and memory-pressure the sidecar; the Rust kernel and PTYs must
  remain healthy and the sidecar must recover without replaying effects.
- Verify allow, deny, malformed, reordered, disconnected, and unavailable
  permission-broker cases fail closed.
- Prove commands supporting `--cached-only --frozen` work with all network
  interfaces unavailable, and separately prove LSP and long-lived modes remain
  offline with prewarmed caches and OS containment.
- Prove `run`, FFI, native addons, undeclared environment, and out-of-scope file
  access are denied by both policy tests and OS containment.
- Sign the nested executable before signing/notarizing the desktop bundle, and
  verify update checksum, atomic replacement, and rollback.
