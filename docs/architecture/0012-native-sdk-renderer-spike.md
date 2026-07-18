# ADR 0012: Keep the Tauri baseline and evaluate Native SDK as a renderer spike

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md),
  [ADR 0010](0010-deno-first-static-workbench-build.md),
  [ADR 0011](0011-versioned-block-render-document.md)

## Context

Vercel's Native SDK offers a native-first desktop architecture that is relevant
to Hyper Term: declarative UI, deterministic Model/Message/update state,
structural widget identity, virtual lists, native scrolling, record/replay
automation, GPU or software canvas surfaces, and optional WebView islands.

At the audited `v0.5.3` tag and commit
`57bf56bc58058768d436099c1005bfc66bd55ac3`, it is Apache-2.0 and pre-1.0. Its
default release UI contains no browser, WebView, or JavaScript runtime. Native
markup is compiled ahead of time into a Zig application.

The performance claim needs a narrower interpretation:

- macOS native canvas presentation uses Metal and OS scrolling;
- Linux and Windows canvas presentation currently uses the deterministic CPU
  renderer and a platform pixel blit;
- React, editors, and Agentic UI placed in a system WebView still use WKWebView,
  WebKitGTK, or WebView2, as they do behind other system-WebView shells;
- the SDK has no terminal emulator, PTY parser, Rust command layer, or proven
  high-throughput terminal stream.

Replacing Tauri with a Zig host therefore does not automatically make the
React, editor, or terminal hot paths faster. It introduces a Rust/Zig boundary,
a second native toolchain, and a pre-1.0 host lifecycle.

The current official sources are the [Native SDK repository](https://github.com/vercel-labs/native/tree/57bf56bc58058768d436099c1005bfc66bd55ac3),
[Native Surfaces](https://native-sdk.dev/native-surfaces),
[Multiple WebViews](https://native-sdk.dev/webviews),
[Bridge](https://native-sdk.dev/bridge), and
[Platform Support](https://native-sdk.dev/platform-support).

## Decision

Tauri remains the reference desktop adapter for the next implementation phase.
It is not made part of `hyper-term-core`; the kernel and Block Render protocols
remain host-independent.

Native SDK will be evaluated as a separate, macOS-first renderer/composition
spike. It is not yet selected as the product shell, PTY owner, permission broker,
agent runtime, or cross-platform renderer.

```text
                        ┌────────────────────────────┐
                        │ Rust hyperd / core         │
ACP/MCP/PTY/Deno ──────>│ journal + policy + blocks │
                        └──────────┬─────────────────┘
                                   │ versioned local protocol
                     ┌─────────────┴─────────────┐
                     │                           │
             Tauri reference client      Native SDK spike client
             React/WebView baseline       native Block canvas
             isolated preview             terminal surface experiment
                                          bounded WebView islands
```

For the Native spike, Rust runs as an out-of-process `hyperd`. A crash or reload
of the Zig shell must not kill PTYs, agents, approvals, or history. The spike
does not link `hyper-term-core` directly into Zig until a later ADR evaluates a
stable C ABI, allocator ownership, thread affinity, panic behavior, and upgrade
compatibility.

## Native SDK role

The Native SDK client may own:

- desktop windows, titlebar, menus, focus routing, notifications, and surface
  composition;
- the virtualized native Block list and precompiled Block renderers from ADR
  0011;
- optional native terminal and Computer Use overlay experiments;
- positioning a small number of trusted or isolated WebView islands;
- renderer-only automation, accessibility snapshots, screenshots, and visual
  replay checks.

It may not own:

- PTYs, process groups, ACP/MCP drivers, Deno supervision, credentials, workspace
  writes, permission decisions, accepted artifacts, or durable operation history;
- hidden execution through Native SDK effects or bridge commands;
- canonical task state that cannot be reconstructed from Rust snapshots and
  patches.

Native SDK's own deterministic journal is renderer evidence, not a second
operation journal.

## Composite Block rendering

The spike is canvas-first. Wrapping the complete existing React workbench in one
main WebView would preserve almost all WebView cost while adding Zig and would
not test the native hypothesis.

The first composition is:

- native virtual list, message/tool/plan/approval blocks, attention chrome, and
  session navigation;
- one terminal block using either the WebView baseline or a purpose-built cell
  grid renderer;
- trusted WebView islands for Tiptap and CodeMirror/Monaco only when selected;
- a separate WebView with bridge disabled for an accepted Agentic UI artifact;
- native placeholders or snapshots for off-screen WebView blocks.

A `WebViewBlock` is a logical child of the Block tree but, on desktop, may be a
separate OS child surface layered over the native canvas rather than pixels
inside that canvas. The native layout engine owns its Block slot and publishes a
clipped logical rectangle; the surface compositor applies position, scale,
visibility, z-order, and focus lease. Scroll and resize generations make stale
geometry updates rejectable. Only visible or explicitly selected blocks receive
a live surface, so virtualization never drags a WebView for every historical
row.

Native markup remains fixed application code. ACP or an agent supplies Block
data, not `.native` source. Runtime-generated custom layout continues through
the validated ADR 0004 UI IR or isolated React artifact.

The system WebView engine is the default. CEF is excluded from this spike: it
increases footprint, is currently available only on macOS in Native SDK, and
would confound the comparison with Tauri's system WebView baseline.

## Data planes

Native SDK's JavaScript bridge is a policy-controlled JSON request/response
channel with a 16 KiB message and response bound and a 12 KiB handler-result
bound. It is suitable for low-rate control intents, not PTY output, model token
streams, source maps, or compiled artifacts.

The spike therefore separates:

1. **Control plane:** framed, schema-versioned local IPC for Block snapshots,
   patches, action intents, acknowledgements, capability negotiation, and
   recovery.
2. **Terminal plane:** ordered binary cell-grid snapshots and damage updates,
   with sequence, backpressure, resize generation, and snapshot recovery.
3. **Artifact plane:** content-addressed files or read-only resources referenced
   by digest; large bytes do not ride the control bridge.
4. **WebView bridge:** narrow, origin-scoped commands for the trusted editor;
   isolated previews use `bridge: false` and the ADR 0004 message protocol.

The exact Rust/Zig IPC encoding and transport are spike outputs. Unix domain
sockets with bounded frames are the initial macOS candidate. Shared memory is
considered only after measurement proves copies are material and its lifetime
and crash-recovery protocol is specified.

## Terminal comparison

The shell and renderer hypotheses are measured separately with the same Rust
PTY/cell model and workload:

1. Tauri plus the current WebView terminal renderer establishes the reference.
2. Native SDK plus the same WebView renderer measures shell/composition cost.
3. Native SDK plus a native cell-grid surface measures renderer cost.

The native path consumes cells and damage records, not full RGBA frames. Sending
an entire terminal as a media texture would waste bandwidth and discard text
selection, hyperlink, accessibility, cursor, and IME semantics.

Native SDK has no existing terminal widget, so the spike must implement or
adapt glyph atlas, shaping, selection, cursor, damage, scrollback, CJK, emoji,
IME, accessibility, and link hit testing before its latency result is comparable.

## Platform and maturity constraints

- macOS is the first spike because it is Native SDK's deepest platform and has
  Metal canvas presentation.
- Linux and Windows remain adoption blockers until the same fixtures prove the
  CPU renderer or a future GPU backend meets the budget. A macOS win is not a
  cross-platform decision.
- System-WebView focus, IME, z-order, accessibility, resize, and crash recovery
  must be tested where native canvas and WebView islands meet.
- Child WebViews are globally bounded by Native SDK; Hyper Term imposes a lower
  measured pool limit and never creates one per Block. At the audited release,
  Native SDK defaults to a global cap of 16 child WebViews; the Block protocol
  negotiates backend capabilities instead of hard-coding that number.
- Zig 0.16 and the exact Native SDK tag are pinned for every spike result.
- The current Native SDK CLI is distributed through npm and its default
  TypeScript core uses a Node development path. The spike uses a Zig core and
  treats the pinned CLI as an external build input; it does not reintroduce
  Node, Vite, or npm services into the packaged application or Workbench runtime.
- Native SDK packaging, signing, updates, Deno sidecar staging, and multi-platform
  installers are evaluated independently from renderer frame time.

## Replacement gates

Native SDK may replace the Tauri reference adapter only after one reviewed ADR
shows all of the following with reproducible fixtures:

- Rust `hyperd` supervision, reconnect, protocol migration, authentication, and
  shell-crash survival with no lost PTY or approval;
- an ordered, bounded Rust/Zig streaming transport with snapshot recovery and no
  silent loss at terminal and model burst rates;
- terminal correctness for CJK, emoji, ligatures, IME, selection, hyperlinks,
  accessibility, resize races, alternate screen, and large scrollback;
- native/WebView focus, keyboard lease, drag/drop, clipboard, menus, multiple
  windows, isolated origins, cross-profile pool isolation, stale-lease
  rejection, deterministic teardown, and bridge denial;
- Deno distribution, signature verification, sandbox profile, lifecycle, and
  update compatibility equal to ADR 0003;
- reproducible acquisition and verification of the Native SDK CLI and Zig
  toolchain without a floating npm or GitHub default-branch dependency;
- signed and updateable macOS, Windows, and Linux artifacts with documented
  rollback and supported WebView prerequisites;
- measured benefit over the Tauri baseline on all target platforms.

The benchmark matrix includes:

- cold/warm startup and first interactive frame;
- 200x60 terminal rendering, resize storms, and a bounded 100 MiB output burst;
- p50/p95 key-to-present and ACP-event-to-Block-present latency;
- CPU, GPU, RSS, allocation/copy volume, dropped or reordered frames, and main-
  thread tasks over 50 ms;
- 100,000 variable-height Blocks, rapid tool updates, attention transitions,
  and WebView pool mount/eviction;
- 1, 5, and 20 MiB accepted artifacts plus `esbuild-wasm` rebuilds;
- killing the renderer, Rust daemon, Deno, and preview independently.

## Consequences

Hyper Term can learn from Native SDK's native canvas, structural identity,
surface composition, and deterministic automation without betting the control
kernel on an immature host. The Block protocol makes the comparison real: both
clients consume identical semantic state and emit identical action intents.

The cost is maintaining a second experimental client and a Rust daemon protocol.
The spike is stopped if the shell-only comparison shows no meaningful benefit,
if cross-platform rendering cannot meet the budget, or if the Rust/Zig boundary
becomes a second authority.

## Rejected alternatives

- **Replace Tauri immediately.** Current evidence does not cover terminal
  correctness, Rust integration, cross-platform GPU rendering, distribution, or
  long-lived sidecars.
- **Run the whole React workbench in Native SDK's main WebView.** It changes the
  shell but not the main rendering architecture and cannot justify the added
  Zig boundary by itself.
- **Move PTY and ACP into Zig effects.** It violates the Rust authority and
  persistence boundaries and creates two lifecycle models.
- **Use the 16 KiB JSON bridge for streams.** Chunking bulk terminal and artifact
  traffic through a request/response control bridge adds serialization and
  backpressure risk without a recovery protocol.
- **Generate Native markup from the agent at runtime.** Release markup is AOT
  application code; treating it as untrusted UI collapses code and data.
