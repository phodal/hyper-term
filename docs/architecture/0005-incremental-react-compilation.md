# ADR 0005: Compile React artifacts incrementally with esbuild-wasm

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0003](0003-brokered-deno-sidecar.md),
  [ADR 0004](0004-versioned-agentic-ui-artifacts.md)

## Context

Agentic UI must feel editable, not generated in a distant build loop. A user or
agent should be able to change a component, see a preview, inspect the error at
the original TSX location, and keep the last working revision when a new one is
invalid.

The prior Canvas Compiler work demonstrates the required boundary:

- browser compilation receives an explicit virtual filesystem and build engine;
- `esbuild-wasm` is initialized once and reused;
- compiler traces, source maps, manifests, and runtime error mappings are output;
- Time Travel instrumentation can wrap React state setters and reducer dispatch;
- the Piece compiler narrows an edit to a declaration graph and demanded preview
  closure, with a conservative full-file fallback.

The relevant snapshots are the
[Canvas Compiler contract](https://github.com/phodal/arch-visual/blob/598dfe0d9ce571f76bc776d802b772968c9a6cb6/packages/canvas-compiler/README.md)
and [Piece incremental architecture](https://github.com/phodal/piece/blob/2be43d13c618eb3de422b5975f9946b6be84fe88/docs/incremental-feedback-architecture.md).
Their benchmark numbers are evidence for the shape of the optimization, not a
performance promise for Hyper Term.

## Decision

The first interactive compiler backend is a persistent `esbuild-wasm` Web
Worker in the trusted WebView. A Web Worker is a performance and scheduling
boundary, not a security boundary: it shares renderer origin capabilities such
as network and storage unless CSP and host policy remove them. It runs only
reviewed compiler code and plugins. User or agent TSX remains input bytes for
parsing and bundling and is never evaluated in this worker. The worker receives
a bounded virtual source snapshot and emits an `ArtifactCandidate` for Rust to
validate under ADR 0004; generated code executes only in ADR 0004's isolated
preview realm.

```text
User or Agent edit
  -> immutable source revision
  -> select a conservative module-slice or full-build path
  -> transform changed modules and update the dependency graph
  -> stable virtual modules + explicit package capsules
  -> cached module registry, or persistent esbuild-wasm context.rebuild()
  -> JS, CSS, source map, diagnostics
  -> bounded local preview candidate
  -> isolated preview switches to the last good revision
  -> approved publish performs a complete Deno build and Rust acceptance
```

Module graph maintenance and conservative fallback belong to Hyper Term's
Canvas/Piece compiler layer; they are not esbuild features. Declaration parsing,
public-shape fingerprints, and declaration-local reverse-closure selection are
still a finer-grained future optimization, not a property of the current
module-slice implementation.

`esbuild.initialize({ worker: false })` runs once inside the dedicated worker,
avoiding a nested worker. Virtual module paths remain stable across revisions.
For the bounded static ESM subset, each TS/TSX/JS/JSX/JSON module is transformed
to a cached CommonJS factory and the Worker rapidly composes a fresh isolated
module registry. Only source-changed modules are transformed. Semantics-sensitive
features such as dynamic `import()`, `import.meta`, source CommonJS, CSS
`@import`, and CSS `url()` use the shared full-build context instead. A newer
edit awaits cancellation of old work before starting the next build; every
request and result carries a source revision, and stale results are discarded.

The compiler never executes application source during analysis or compilation.
Its virtual filesystem has no implicit host filesystem or network fallback.
React, React DOM, the approved `@hyper/ui` registry, and heavy trusted editor
components are host-provided externals rather than repeated bundle inputs.

The compiler produces:

- output bytes and content digests;
- source maps that retain generated and user-authored source identities;
- esbuild metafile and bounded diagnostics;
- dependency, import, transform, and fallback decisions;
- compiler, WASM, registry, lockfile, and option versions;
- Time Travel instrumentation locations and trace schema version.

Only a bounded, digest-checked candidate replaces the local isolated preview.
This browser candidate is not an authoritative publication: publishing an
edited artifact still crosses the approval endpoint and is rebuilt by the
Rust-supervised Deno compiler before Artifact Store acceptance. Compilation or
runtime failure keeps the last-known-good artifact visible and attaches
source-mapped diagnostics to the failed revision.

## Incremental correctness contract

The optimized path is allowed only when it is observationally equivalent to a
clean build of the same selected preview target.

- An edit wholly inside a known declaration updates that declaration and the
  reverse transitive closure demanded by the target.
- Stable content and public-shape fingerprints permit early cutoff and artifact
  reuse.
- A changed import, export, side effect, ambiguous rename, or crossed declaration
  boundary falls back to file or project scope.
- esbuild context options are immutable after creation. A changed target,
  external set, loader, registry, package lock, or compiler configuration selects
  a context keyed by the complete configuration or disposes and creates one; it
  never reuses an incompatible context.
- Full builds are correctness or benchmark fallbacks and never run synchronously
  on every keystroke.
- Cache keys include source, graph, build options, compiler, dependency lock,
  registry schema, and target WebView capabilities.

## Deno's compiler role

The Deno sidecar performs cold-path computations: LSP, formatting, type
checking, package graph and lock preparation, tests, and export-time
verification. Rust validates, accepts, and persists any resulting source,
lockfile, cache, or artifact revision. Deno does not sit between each keystroke
and preview.

The shared host abstraction is conceptually:

```rust,ignore
trait UiCompiler {
    async fn compile(&self, request: CompileRequest) -> CompileResult;
}
```

`EsbuildWasmWorker` is the reference interactive backend. Native esbuild or a
Deno bundler may be evaluated behind this boundary for full builds, but source
and artifact protocols do not depend on them. Deno's experimental bundler is
not selected as a production contract.

esbuild documents the [browser Web Worker API](https://esbuild.github.io/api/#in-the-browser)
and [incremental rebuild API](https://esbuild.github.io/api/#rebuild). It also
warns that the WASM build can be substantially slower than native esbuild, so
the backend remains replaceable.

## Performance and reliability gates

- For a declaration-local warm edit, target p95 edit-to-accepted-preview at or
  below 100 ms on the reference project and hardware; always publish the actual
  fixture, machine, and cold/warm state.
- Benchmark initial and consecutive rebuilds for 100, 500, and 1,000 virtual
  modules, including package capsules and source maps.
- No compiler work may create a UI main-thread task longer than 50 ms during
  typing or agent streaming.
- Compare slice rebuild and clean build outputs in randomized edit tests; an
  uncertain incremental result must fall back, never guess.
- Measure worker startup, WASM initialization, cache size, memory, cancellation,
  stale-result rejection, and error recovery.
- Prove hostile source bytes are never evaluated by the compiler worker, cannot
  resolve undeclared packages, and only execute in the isolated preview. Prove
  CSP, origin separation, and OS policy deny unintended network, storage, host
  file, and native-bridge access rather than relying on Worker isolation.
- Verify source-mapped compile and runtime errors navigate to the exact source
  revision and callsite.

## Consequences

The hot path follows a previously exercised browser-contained Canvas boundary
and keeps the Deno sidecar off the render latency path. The system now has both
WebView and Deno JavaScript runtimes. Hot and cold paths are separated, and any
intentional overlap in transforms or full-build validation is mediated by the
`UiCompiler` and artifact protocols. A native compiler can replace the worker
if measured projects exceed the WASM budget without changing artifact history.

## Implementation evidence (2026-07-19)

The browser Worker and the supervised Deno compiler now import one
`compiler-engine.ts`. Both use the same bounded virtual filesystem, React
capsules, esbuild-wasm version, external source-map output, diagnostic schema,
and `ArtifactCandidate` digest. The Worker remains the persistent keystroke hot
path. Deno is the approved cold-path verifier exposed through MCP; Rust checks
the request/response revision, compiler identity, output bounds, and digest
before the result becomes a receipt.

The cold-path receipt now remains provisional until the daemon accepts the
candidate for the exact dispatching operation. The Rust artifact store and
Block projector preserve one last-known-good accepted artifact across a rejected
candidate and daemon restart. An authenticated preview shell renders only that
current artifact, while source-map bytes remain behind a separate task-bound
endpoint.

This proves backend parity on a real single-file React compile and closes the
first last-known-good acceptance/delivery slice.

## Incremental rebuild evidence (2026-07-21)

The shared compiler engine now holds one `esbuild.context()` for a compatible
entrypoint and virtual-file inventory and applies source edits through
`context.rebuild()`. A changed entrypoint or file inventory disposes that
context before creating a new one, so incompatible plugin resolution state is
never reused. This applies to both the browser Worker and the supervised Deno
cold path because they import the same engine.

The Worker serializes rebuilds, calls `context.cancel()` when a newer revision
arrives, and retains only the newest waiting request. Every displaced request
receives a typed `compile_superseded` response, so browser promises do not leak
or wait for their timeout. Rust's Deno protocol remains unchanged: the browser
scheduling response cannot cross the daemon authority boundary.

Unit tests cover context reuse, deterministic disposal, cancellation, and
queued-edit coalescing. The ignored Rust integration test compiles three real
artifacts through pinned Deno and `esbuild.wasm`: an initial TSX snapshot, a
same-inventory rebuild, and a two-file inventory change.

Module-slice invalidation, deterministic clean-build equivalence, hostile-runtime
recovery, and scale measurement are now implemented below. Broader randomized
syntax generation, declaration-local invalidation, and memory/cache-pressure
measurement remain open gates.

## Warm interactive evidence (2026-07-22)

The built Workbench browser gate now drives CodeMirror through its real input
path and measures every accepted revision across the esbuild-wasm Worker, host
acceptance, and authenticated preview iframe `ready` message. The diagnostic
surface retains at most 64 timing-only samples and 128 long-task observations;
it exposes no source, path, bundle, or diagnostic content. A new edit advances
the revision immediately, so a cancelled or stale compile cannot satisfy a
later sample.

On a Mac Studio `Mac16,9` with an Apple M4 Max (16 cores, 64 GB), macOS 26.5.2,
Chrome for Testing 147.0.7727.56, and agent-browser 0.25.4, twelve warm edits of the
single-module TSX fixture produced 28.3 ms p50 and 33.4 ms p95/max
edit-to-preview latency. The fixture replaces the complete module on each edit,
which is more work than the declaration-local target. No overlapping main
thread task reached 50 ms. The release gate fails when warm p95 exceeds 100 ms,
when any sample is cold or missing, or when a main-thread long task overlaps a
sample.

This closes the reference warm single-module p95 and main-thread long-task
slice. Initial Worker/WASM startup, memory and cache bounds, broader randomized
syntax equivalence, and acceptable 100/500/1,000-module rebuild latency remained
required at this checkpoint; the module-scale latency is closed below.

## Scale evidence (2026-07-22)

The bounded source contract now accepts at most 1,000 virtual files while
retaining the 1 MiB aggregate budget. UTF-8 virtual paths count toward that
budget and each path remains independently bounded. Browser validation, the
Rust Deno compiler, Artifact persistence, editor checkpoints, draft publishing,
and Deno LSP use the same Rust protocol constants; 1,001 files fail before
compiler or persistence work begins.

The release browser gate creates the exact production `compiler.worker.js` and
builds complete linked graphs with external source maps. The first 100-module
sample includes Worker and WASM initialization; the later initial samples create
new esbuild contexts in the already-warm Worker. Five same-inventory leaf edits
then exercise `context.rebuild()` at each scale.

Two consecutive successful runs produced these ranges:

| Modules | Initial | Rebuild p50 | Rebuild p95/max | Source map |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 1,081.8–1,105.1 ms | 1,046.8–1,055.6 ms | 1,090.0–1,973.3 ms | 15,615 B |
| 500 | 5,130.2–5,169.3 ms | 5,018.7–5,063.1 ms | 5,087.6–5,194.1 ms | 74,414 B |
| 1,000 | 10,272.6–10,278.7 ms | 10,171.9–10,205.8 ms | 10,275.3–11,041.7 ms | 147,914 B |

All source maps contained every requested module and no main-thread long task
was observed. In twelve-revision 1,000-module bursts, nine to ten queued
revisions were reported as `compile_superseded` and the final revision compiled
successfully.

This benchmark closed the missing scale measurement, but it also disproved that
the full-graph esbuild-wasm rebuild was interactive at 500 or 1,000 modules. It
triggered the module-slice implementation measured below.

## Module-slice evidence (2026-07-22)

The production browser Worker now selects a conservative module-slice compiler
for static ESM graphs. It caches transform output by virtual path and exact
source, reconstructs a bounded module registry on every revision, concatenates
reachable CSS in dependency order, and emits an indexed source map. Runtime
diagnostics flatten that map before resolving a generated position. The Deno
compiler continues to use the complete bundled build; publication behavior and
Rust Artifact Store authority are unchanged.

The release browser gate compiled complete linked graphs, changed only the leaf
module five times, then sent a twelve-revision 1,000-module burst:

| Modules | Initial | Rebuild p50 | Rebuild p95/max | Source map |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 109.0 ms | 1.2 ms | 1.3 ms | 20,890 B |
| 500 | 365.0 ms | 2.8 ms | 4.0 ms | 104,607 B |
| 1,000 | 468.9 ms | 5.5 ms | 6.9 ms | 209,607 B |

The same built-page run produced 6.9 ms p50 and 14.0 ms p95/max
edit-to-preview latency with no main-thread long task. Nine of twelve queued
burst revisions were superseded and the last revision compiled. Unit coverage
executes static TypeScript, JSON, simple CSS, safe circular ESM, missing
dependency rejection, indexed runtime mappings, and eight deterministic
branched graphs against both the slice and clean full-build engines. The browser
gate also exercises React capsules and the isolated hostile-preview denial path.

The standalone cold-process benchmark includes a new Deno process and WASM
initialization. At 1,000 modules it measured 4,248.4 ms cold, 19.1 ms warm p50,
and 26.2 ms warm p95/max. This is below the editor timeout and confirms that the
fast result is not dependent on an already-created module cache. These numbers
are reference-machine evidence, not a cross-machine guarantee.
