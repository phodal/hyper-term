# ADR 0010: Build the static workbench with Deno and exclude Vite

- Status: proposed
- Date: 2026-07-18
- Refines: [ADR 0003](0003-brokered-deno-sidecar.md),
  [ADR 0005](0005-incremental-react-compilation.md)

## Context

The disposable desktop spike predates the Deno architecture and currently uses
Vite to serve and bundle the trusted React workbench. Its Tauri configuration
loads `http://127.0.0.1:1420` during development, and its CSP permits the matching
HTTP and WebSocket endpoints. This is a property of the spike, not an accepted
runtime or compiler boundary.

Leaving that distinction implicit is misleading. Hyper Term has three different
build planes with different trust, latency, and lifecycle requirements:

1. building the trusted React workbench shipped inside the desktop application;
2. compiling user- or agent-authored Agentic UI while the application is running;
3. loading already-built assets in a packaged application.

Vite's development server, plugin lifecycle, module graph, client, and HMR
protocol are not needed in any product-runtime boundary. In particular, applying
HMR directly to generated UI would mutate a live module graph outside the
immutable source revision, artifact acceptance, and Time Travel contracts.

Deno's current toolchain exposes an experimental `deno bundle` command for
2.4 and newer, powered by esbuild; Deno 2.5 added HTML entry points. It supports
browser targets, source maps,
code splitting, CSS, watch mode, React, npm/JSR resolution, and programmatic
in-memory output. Tauri can load a static `frontendDist` and, when `devUrl` is
omitted, provide a development server for that directory. These capabilities are
enough to test a Deno-first, server-independent workbench build without adopting
Vite as an architectural dependency.

See Deno's current [bundling reference](https://docs.deno.com/runtime/reference/bundling/)
and Tauri's [`devUrl` and `frontendDist` configuration](https://v2.tauri.app/reference/config/#buildconfig).

## Decision

Hyper Term will not use Vite in the target architecture. Vite is excluded from
the packaged runtime, trusted-workbench build contract, Agentic UI compiler,
preview protocol, and target test dependency graph. Its presence in the current
desktop spike is temporary and must not be used as evidence for the target
design.

The three build planes are assigned as follows:

| Plane | Owner and mechanism | Runtime network service |
| --- | --- | --- |
| Trusted workbench build | A pinned Deno toolchain produces static, content-addressed browser assets ahead of time. | None in a packaged application. |
| Agentic UI compilation | A persistent `esbuild-wasm` Worker compiles a bounded virtual filesystem into an `ArtifactCandidate`; Rust validates and accepts it. | None; compilation and artifact delivery use typed local host channels. |
| Packaged workbench load | Tauri embeds and loads the accepted static `frontendDist`; Rust projects durable state into React. | None. |

### Trusted workbench build

`deno task` is the only JavaScript/TypeScript task entry point. The first build
adapter invokes the exact pinned Deno release's HTML bundler with a browser
target, locked dependencies, source maps, and deterministic output metadata. A
conceptual release build is:

```text
deno task check
  -> deno bundle --platform=browser --outdir=dist index.html
  -> verify hashes, source maps, dependency lock, and forbidden imports
  -> cargo tauri build embeds dist
```

The command spelling is an implementation detail behind a small
`WorkbenchBuilder` contract. Because Deno's bundler remains experimental, the
application pins the complete Deno version and records it in build provenance.
If a measured feature or correctness gap blocks the HTML bundler, the fallback
is a reviewed Deno build adapter using pinned esbuild directly. The fallback may
not introduce Vite, a Node daemon, plugin auto-discovery, or an application
runtime dependency.

Development uses the same adapter in watch mode and writes static assets to
`frontendDist`. Tauri's directory development server may reload the trusted
workbench when those assets change; the architecture does not depend on React
Fast Refresh or module-level HMR. A full workbench reload reconstructs terminal,
agent, document, approval, and artifact state from the Rust kernel.

The bundled Deno sidecar described by ADR 0003 is not a boot-time frontend
compiler. Release assets are built before packaging. At runtime Deno starts only
for explicitly requested tooling such as LSP, formatting, checking, or extension
evaluation.

### Agentic UI compilation

ADR 0005 remains the hot-path decision. Generated TSX is compiled by the
persistent `esbuild-wasm` Worker from an explicit virtual filesystem. It does
not pass through `deno bundle`, Vite, a dev server, a localhost URL, a Vite
client, or an HMR WebSocket.

Every preview transition remains:

```text
immutable source revision
  -> esbuild-wasm output
  -> ArtifactCandidate
  -> Rust policy and manifest validation
  -> accepted artifact ID
  -> isolated preview load
  -> semantic trace events
```

Changing the preview without a newly accepted artifact ID is a protocol error.
This keeps compile failures, runtime failures, approvals, and Time Travel
evidence attached to a single reproducible revision.

### Dependency and configuration boundary

The target workspace uses `deno.json` tasks and `deno.lock` for the trusted
React toolchain. Rust/Cargo remains the top-level native build authority. The
desktop target must not depend on `vite`, `@vitejs/*`, `vite/client`, or a test
runner that imports Vite transitively.

Production configuration contains no frontend `devUrl`, localhost development
origin, HMR client, or HMR WebSocket CSP grant. Any temporary compatibility
configuration needed while replacing the disposable spike is development-only
and cannot enter a release bundle.

## Consequences

The workbench and generated UI share esbuild semantics without sharing a mutable
dev-server module graph: Deno owns the ahead-of-time trusted build, while
`esbuild-wasm` owns the bounded interactive compiler. The packaged application
starts fewer services and can render its shell before Deno is started.

We give up Vite plugins, React Fast Refresh, and its mature development server.
Workbench development may initially use full reloads, but Rust-owned session
state makes those reloads recoverable. The Deno bundler's experimental status
requires an exact version pin, reproducible-build fixtures, and a replaceable
adapter rather than allowing its API to leak into application code.

## Rejected alternatives

- **Keep Vite only for development.** This preserves a second package, plugin,
  module-resolution, dev-server, and test ecosystem and makes the repository's
  actual architecture ambiguous.
- **Ship or start a Vite server with the application.** Product rendering must
  not depend on localhost, a port, HMR, or a JavaScript server lifecycle.
- **Use Deno bundling for every Agentic UI edit.** It puts a cold-path tool
  process between typing and preview and weakens the explicit virtual filesystem
  and artifact-candidate boundary.
- **Let generated UI participate in workbench HMR.** It can mutate executable
  state without creating the immutable artifact transition required by Rust and
  Time Travel.

## Validation gates

- A packaged application launches and renders the workbench while Deno is not
  running and without binding or connecting to a frontend network port.
- The trusted workbench builds from a clean checkout with the pinned Rust and
  Deno toolchains and no Node, pnpm, Vite, or Vitest executable.
- Dependency inspection finds no Vite package in the target desktop dependency
  graph, including transitive test dependencies.
- Killing and reloading the trusted workbench reconstructs active terminal and
  agent state from Rust rather than relying on HMR-preserved JavaScript memory.
- Agentic UI preview changes only after Rust accepts a new content-addressed
  artifact, and its CSP denies localhost, network, native bridge, and HMR access.
- Clean and consecutive Deno workbench builds are byte-for-byte reproducible
  after excluding declared build metadata such as timestamps.
