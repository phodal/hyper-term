# ADR 0015: Capture Native UI as versioned scene snapshots

- Status: proposed
- Date: 2026-07-19
- Depends on: [ADR 0011](0011-versioned-block-render-document.md),
  [ADR 0013](0013-native-sdk-default-product-shell.md), and
  [ADR 0014](0014-rust-owned-coding-agent-sandbox.md)

## Context

Hyper Term's README image is generated from the real desktop application module
and Native markup. The current adapter builds `main.zig` and `app.native`, lays
out the resulting widget tree with Native SDK, emits a `DisplayList`, and passes
that list to Native SDK's SVG exporter.

This keeps the image closer to the product than a hand-drawn architecture
diagram, but the generator always starts from `initialModel()`. That model has
one ordinary `zsh` tab, so the image cannot show the product's distinguishing
Agent and ACP states even though those states are already rendered by the same
native view.

The existing command also writes SVG directly. It does not preserve the scene
that produced the SVG, cannot capture a running retained-canvas view, and does
not provide a review/update workflow comparable to React snapshot tests.
Consequently, a README image can be reproducible while still exercising only an
uninteresting application state.

Native SDK already supplies most of the required primitives:

- a retained `CanvasFrame` and `DisplayList` for every `gpu_surface` view;
- a deterministic CPU reference renderer used by automation screenshots;
- the versioned, app-neutral `native.canvas.scene` JSON format;
- SVG export from a canvas scene, including deterministic raster fallback for
  effects that SVG cannot preserve directly;
- automation addressing by stable view label and native widget context-menu
  actions.

These primitives create an opportunity broader than the README. A Native SDK
application should be able to capture its current semantic/vector scene once,
then use the same artifact for visual tests, documentation, bug reports, and
additional exporters.

An arbitrary bitmap is not that artifact. OCR, tracing, or model-based image
reconstruction can approximate shapes and text, but cannot recover widget
identity, layout constraints, resources, accessibility semantics, or trusted
interaction boundaries. Hyper Term will therefore capture before rasterization
rather than convert pixels back into an invented Native tree.

## Decision drivers

- The README must show a deterministic ACP-backed Agent state, not depend on a
  locally installed or authenticated provider.
- Documentation must follow the real `.native` view, Zig model, design tokens,
  layout, and display-list pipeline.
- The capture mechanism should be reusable by other Native SDK applications and
  future exporters.
- Snapshot review must distinguish intentional UI changes from stale generated
  assets and require an explicit update.
- Terminal, ACP, MCP, and Agent output remains untrusted data and must never be
  interpreted as executable Native markup or a file operation.
- Native canvas and child WebView surfaces must remain honest, separate
  rendering planes.
- Hyper Term's Rust-owned permission and persistence boundary must not move into
  a WebView or agent-generated action.

## Decision

Hyper Term will adopt **Native Scene Snapshots** as the source for generated UI
documentation and visual regression checks.

A scene snapshot captures the current native presentation after model projection
and layout but before rasterization:

```text
deterministic fixture                         running application
        |                                            |
Model + compiled .native view              retained gpu_surface view
        |                                            |
widget tree + DesignTokens                  current CanvasFrame
        |                                            |
layout + DisplayList -------------------------------+
                             |
                  native.canvas.scene JSON
                     /        |        \
          semantic snapshot  SVG    reference PNG
```

The two capture paths converge on the same app-neutral scene document:

- **Fixture capture** builds a known application model and is the authority for
  committed documentation and regression snapshots.
- **Runtime capture** reads the current retained frame and supports automation,
  debugging, and an explicit user-facing export action.

Neither path converts a PNG into Native SDK objects.

## Ownership

Native SDK owns the generic mechanism:

- capture a retained canvas view by window and stable view label;
- construct a `SceneJsonDocument` from the current frame, including the display
  list, dimensions, clear color, images, and fonts required for replay;
- write canonical `native.canvas.scene` JSON;
- derive SVG and deterministic reference PNG from that scene;
- expose capture through a reusable library API and automation command;
- validate format version, resource limits, paths, and output names.

Hyper Term owns product-specific inputs and policy:

- named scenarios such as `terminal-default` and `agent-acp-review`;
- bounded, synthetic Agent/ACP block fixtures with stable identifiers;
- the README choice of scene and accessible description;
- the right-click/menu command that asks for a capture;
- persistence through the trusted host and Rust permission boundary when a user
  selects a destination.

The Hyper Term adapter may assemble the real model and view, but it must not
fork Native SDK's scene schema, layout engine, SVG writer, or reference
renderer.

## Snapshot artifact contract

Each named snapshot has a small manifest and one canonical scene:

```text
snapshots/agent-acp-review/
  snapshot.json
  scene.json
  semantics.txt
  render.svg
```

`snapshot.json` records the snapshot schema version, scenario name, Native view
label, logical dimensions, color scheme, and digests of the canonical scene and
semantic projection. It does not contain credentials, loopback tokens, provider
environment, timestamps, absolute workspace paths, or live terminal history.

`scene.json` is the authoritative visual artifact and uses the Native SDK
`native.canvas.scene` schema. It contains only the bounded commands and
resources needed to replay the canvas.

`semantics.txt` is a normalized accessibility/widget summary. It catches changes
that can be invisible in a pixel comparison, such as a missing label, role,
selected state, or approval action.

`render.svg` is tracked when documentation references it. Reference PNG output
is generated for image comparison and CI evidence, but need not be committed
unless a consumer specifically requires a raster asset.

All committed output must be byte-stable for an unchanged fixture, SDK version,
font set, target dimensions, and color scheme. Snapshot updates are explicit;
ordinary checks never rewrite the baseline.

## Hyper Term ACP/Agent fixture

The first new scenario is `agent-acp-review`. It uses the real application model
and renderer with synthetic data representing:

- an ordinary `zsh` tab alongside an active `Codex ACP` Agent tab;
- an ACP-backed Agent response;
- a compact plan or tool-call result;
- an exact operation proposal and visible approval state;
- the Rust-owned safety status shown by trusted native chrome.

The fixture must not start `hyperd`, contact a provider, read a workspace, use
account credentials, or depend on network access. It projects a bounded static
Block snapshot through the same model update and `rootView` path used by the
desktop application. IDs and revisions are fixed fixture values.

The README hero image will be derived from this scenario so the first product
visual communicates the Agent/ACP workflow. `terminal-default` remains a
separate regression scenario proving that a normal terminal is still the default
product state.

## Snapshot workflow

Hyper Term will expose explicit update and verification tasks conceptually
equivalent to React snapshot tests:

```text
deno task snapshot:update -- agent-acp-review
deno task snapshot:check
deno task render:readme
deno task check:readme-svg
```

The exact task names may be introduced incrementally, but their behavior is
fixed:

1. `snapshot:update` regenerates the manifest, canonical scene, semantics, and
   derived render for an explicitly named scenario.
2. `snapshot:check` renders into temporary output and fails with a useful diff
   when tracked semantic or scene artifacts are stale.
3. `render:readme` copies or derives the selected tracked scene into the README
   SVG path.
4. `check:readme-svg` remains read-only and fails if the documented asset does
   not match its scene.

CI will run the check path, never the update path.

## Runtime and right-click capture

Native SDK will expose the same core through a command shaped like:

```text
native snapshot capture <view-label> --name <name> --formats scene,svg,png
```

The final spelling is an SDK CLI concern, but capture must address a live
`gpu_surface` view and serialize its retained frame rather than screenshot the
desktop window.

Hyper Term may add `Export Native Snapshot...` to the native canvas context
menu. That action is only an interaction adapter:

```text
native context-menu selection
  -> trusted Hyper Term message
  -> Native SDK in-memory scene capture
  -> user-selected destination
  -> Rust permission broker authorizes the exact write
```

The WebView cannot invoke an arbitrary output path, write the artifact, or
silently trigger capture. Automation remains compile-time gated and writes only
inside its bounded automation directory. A user-facing export requires an
explicit native action and destination.

## WebView boundary

Native Scene Snapshots cover retained-canvas `gpu_surface` views only. Hyper
Term's native session chrome and Agent/ACP conversation are eligible. Child
WebViews used by the terminal surface or accepted GenUI editor are separate OS
surfaces and are not serialized into `native.canvas.scene`.

If a bug report or documentation page needs the complete composited window, the
host may capture WebView pixels and compose a raster PNG. That output must be
labelled as a composited screenshot. It is not a vector scene, cannot be used as
a Native snapshot baseline, and must not be passed through tracing or OCR and
presented as a faithful SVG reconstruction.

The `agent-acp-review` README scene will keep the distinguishing Agent content
on the native canvas, so it does not require WebView pixels.

## Determinism and security

- Fixtures use stable IDs, revisions, ordering, dimensions, theme, and bundled
  font/resource inputs.
- Runtime-only counters, frame timestamps, process IDs, random ports, absolute
  paths, tokens, and provider metadata are excluded from committed snapshots.
- Live captures are user-owned diagnostic artifacts and may contain visible
  sensitive text; they are never committed automatically.
- Scene parsing retains Native SDK's command, path, glyph, resource-count, and
  resource-byte limits.
- Text and terminal cells remain inert render data. The exporter never parses
  them as Native markup, shell commands, URLs to fetch, or output paths.
- Missing or unsupported resources fail explicitly or use the scene format's
  declared deterministic fallback; they never trigger an implicit network
  request.
- Canonical serialization preserves meaningful display-list order and does not
  hide changes by sorting draw commands.

## Validation

Native SDK validation must cover:

- retained `CanvasFrame` to `SceneJsonDocument` capture;
- scene write, parse, and write round trips;
- SVG generation from the captured scene;
- deterministic PNG generation from the same scene;
- images, registered fonts, clipping, gradients, layers, and declared raster
  fallback;
- invalid view labels, oversized scenes, unsafe output names, and WebView
  rejection;
- byte stability across two captures of the same retained frame.

Hyper Term validation must cover:

- the fixture builds the real `rootView` and Native markup;
- layout and accessibility audits pass at the supported minimum and default
  window sizes;
- the scene contains `Agent`, `Codex ACP`, and at least one tool, plan, or
  approval projection;
- generated SVG is valid XML and matches the tracked scene;
- a reference PNG is non-empty and visually compared with the SVG render;
- no credential, loopback token, absolute workspace path, or live transcript
  appears in tracked artifacts;
- `terminal-default` still renders a normal terminal without requiring an Agent
  provider.

## Rollout

1. Extract a deterministic `agent-acp-review` fixture from the existing Agent
   snapshot tests and teach the Hyper Term adapter to generate its scene and
   README SVG.
2. Add canonical scene and semantic snapshot update/check tasks without changing
   runtime capture.
3. Add the generic retained-frame capture API and automation command to Native
   SDK, with protocol and exporter tests there.
4. Rebase the Hyper Term adapter on the published SDK interface and remove any
   temporary fixture-only scene assembly that duplicates it.
5. Add the optional native context-menu export after the Rust broker has an
   exact, user-selected snapshot-write request.
6. Consider composited PNG capture separately if terminal or GenUI WebView
   evidence becomes a documented requirement.

## Consequences

README visuals and regression tests will exercise recognizable product states
instead of only the default empty terminal. A failing snapshot will point to a
semantic or scene change before reviewers inspect a large generated SVG diff.
The same app-neutral artifact can feed SVG now and additional layout,
inspection, or export tools later.

Native SDK gains a reusable scene-capture capability rather than a
Hyper-Term-specific screenshot hook. Hyper Term remains responsible for what
state is safe and useful to document, while the SDK remains responsible for how
that state becomes a portable scene.

The costs are a versioned snapshot manifest, additional tracked fixture data,
normalization rules, SDK automation protocol work, and explicit review when
rendering changes. A scene snapshot also cannot prove child WebView rendering;
those surfaces continue to require separate browser or composited-raster
verification.

## Rejected alternatives

- **Trace or OCR a PNG into SVG.** This loses semantics, component identity,
  constraints, resources, trust boundaries, and reliable text metrics.
- **Keep the README-only direct SVG generator.** It can remain as an adapter,
  but without a named scene baseline it does not provide reusable runtime
  capture or React-style snapshot review.
- **Capture a live authenticated ACP session for documentation.** It is
  nondeterministic and risks leaking provider, workspace, transcript, and
  credential data.
- **Commit only a PNG baseline.** Pixel diffs do not explain semantic changes
  and prevent future vector or structured exporters from reusing the scene.
- **Serialize the Hyper Term `Model` as the portable format.** It couples the
  exporter to product state and bypasses Native SDK's app-neutral display-list
  boundary.
- **Capture WebView DOM and CSS as Native SDK scene objects.** It creates a
  second browser-to-native compiler, misrepresents OS-composited surfaces, and
  expands the trusted input boundary.
- **Put the ACP fixture in Native SDK.** The SDK should test generic scene
  primitives; Hyper Term owns its Agent vocabulary and safety presentation.
