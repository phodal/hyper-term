# ADR 0004: Represent Agentic UI as versioned artifacts, not native authority

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0002](0002-runtime-authority-boundaries.md),
  [ADR 0003](0003-brokered-deno-sidecar.md)

## Context

An AI-era terminal should be able to generate the interface best suited to the
current task: a deployment checklist, trace explorer, approval form, test
dashboard, table, diagram, or debugger. Treating model-generated HTML or TSX as
part of the trusted application would, however, give unreviewed code the same
origin and native bridge as the terminal.

The product also needs to know exactly which source, compiler, dependency set,
component schema, action contract, and permission decision produced the UI the
user saw. A transient React component is not a sufficient history record.

## Decision

Agentic UI is a content-addressed, versioned artifact with two authoring levels:

1. A declarative UI IR is the default. It maps typed data and actions onto a
   trusted, versioned `@hyper/ui` component registry.
2. Generated TSX is an advanced escape hatch for interactions the IR cannot
   express. It is compiled and executed only in an isolated preview realm.

Source and event revisions are canonical. A JavaScript bundle is a derived,
discardable cache.

Each accepted artifact has a manifest conceptually shaped like:

```text
artifact_id = hash(canonical manifest of every semantic compile input)

semantic inputs
  source tree / entrypoint / dependency lock
  compiler + options / target + WebView capabilities
  component registry / transform + instrumentation policy / limits

UiArtifactManifest
  schema_version
  artifact_id / source_revision / parent_revision
  entrypoint / target / compiler_version / compiler_options_digest
  dependency_lock_digest / component_registry_version
  declared_actions[] / required_view_capabilities[]
  output_digest / source_map_digest / trace_schema_version
  limits / CSP profile / created_by / created_at
```

The output digest is validation data and is not substituted for the semantic
input identity. The compiler returns an `ArtifactCandidate`. Rust validates its
schema, digests, limits, allowed imports, actions, and parent revision before it
becomes an accepted `UiArtifact`. A stale candidate is discarded even if
compilation succeeded. A failed build leaves the last-known-good artifact
visible.

## Preview boundary

Generated TSX executes in a sandboxed iframe, isolated child WebView, or
equivalent separate origin that has:

- no Tauri `invoke` object or raw Rust command channel;
- no Deno, shell, filesystem, credential, clipboard, screen, or network API;
- a strict CSP and no remote script source;
- CPU, memory, message-size, and render-rate limits;
- one schema-validated `postMessage`-style host protocol.

The preview protocol is deliberately small:

```text
Host -> View: Init(snapshot, revision, locale, theme)
Host -> View: Patch(sequence, revision, data_delta)
View -> Host: ProposeAction(action_id, artifact_revision, inputs)
View -> Host: Trace(trace_event, artifact_revision)
View -> Host: RuntimeError(error, source_location)
```

An action declaration is not authority. `ProposeAction` creates or updates a
Rust-owned operation proposal. The permission broker validates its artifact
revision, action schema, target, inputs, and required capabilities. Editing any
of these invalidates an earlier approval.

## Component and package policy

- The declarative registry exposes accessible, bounded components and typed
  action slots, not arbitrary React component imports.
- Generated TSX may import only explicitly externalized React and registry
  modules plus packages admitted by the artifact profile.
- Host components never pass native handles, secrets, or executable callbacks
  into the preview.
- HTML strings and terminal output are rendered as untrusted text unless a
  dedicated sanitizer and content type explicitly permit markup.

## Consequences

This design makes generated interfaces inspectable, replayable, shareable, and
revocable without turning them into plugins with ambient machine access. It
adds a compiler/validator step and prevents arbitrary npm UI packages from
running in the trusted shell. Some sophisticated generated views will require a
new registry component or an explicit elevated artifact profile.

## Rejected alternatives

- **Model emits HTML directly into the main document.** This collapses content
  and authority and makes CSP, event history, and source mapping unreliable.
- **Generated React imports the Tauri API.** A presentation artifact could
  execute effects without a stable operation revision.
- **Persist only the final bundle.** The source, dependency, compiler, and
  action history needed for debugging and reproducibility would be lost.
- **Allow arbitrary remote modules during compile or render.** The same source
  revision could produce different behavior and bypass dependency approval.

## Validation gates

- Attempt native API, network, popup, clipboard, cross-origin, and oversized
  message access from a hostile artifact and prove denial.
- Rebuild an artifact from its source and lock revision and verify all declared
  deterministic output digests.
- Verify a changed source, action input, target, or artifact revision invalidates
  approval and stale compiler results.
- Force compilation and runtime failures and prove last-known-good rendering,
  source-mapped diagnostics, and recovery.
- Accessibility, keyboard, IME, CJK, and screen-reader tests apply to both the
  trusted registry and sandbox focus boundary.
