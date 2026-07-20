# ADR 0006: Implement Time Travel as semantic event replay

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0004](0004-versioned-agentic-ui-artifacts.md),
  [ADR 0005](0005-incremental-react-compilation.md)

## Context

The product goal is not merely to retain shell history. A user debugging an AI-
generated interface needs to know what the human, agent, compiler, UI, tool, and
machine each saw and did. They also need to step back through deterministic UI
state without accidentally running a command, writing a file, calling an MCP
tool, or clicking an application again.

React Fiber, a V8 heap snapshot, a screenshot recording, and a terminal
transcript each capture only part of that history. They are also unstable or
unsafe persistence formats.

## Decision

Time Travel Debug is replay of versioned semantic events from Rust-owned
checkpoints. It does not rewind an operating system process or replay native
side effects.

Hyper Term records three related timelines:

1. **Domain timeline:** tasks, runs, operations, approvals, actor handoffs,
   normalized ACP/MCP lifecycle and domain events, Computer Use observations,
   and effect receipts. Raw protocol frames are optional, bounded, redacted
   evidence references rather than canonical events.
2. **Document timeline:** source edits, Tiptap/ProseMirror steps, code-editor
   transactions, file patches, selection anchors, and immutable revisions.
3. **Artifact timeline:** compiler trace frames, accepted UI artifacts, runtime
   actions, reducer/state instrumentation, console events, and source-mapped
   errors.

All streams use a common envelope:

```text
event_id / stream_id / monotonic_sequence
event_type / schema_version
task_id / run_id / operation_id / artifact_revision
actor / causation_id / correlation_id
recorded_at / logical_time
payload_ref / redaction_profile / integrity_digest
```

Large or sensitive values are content-addressed artifacts with retention and
redaction policy, not unbounded event payloads. The journal stores one ordered
writer per stream and periodically materializes versioned checkpoints. A
projection is rebuilt from the nearest compatible checkpoint plus later events.

## Replay semantics

```text
ReplayView(revision)
  = compatible checkpoint
  + ordered deterministic events through revision
  + recorded effect receipts substituted for external effects
```

During replay, the following are observations only:

- Bash commands, PTY input, process creation, and file writes;
- network requests and provider calls;
- MCP tools and ACP agent effects;
- Computer Use clicks, keys, screenshots, and application changes;
- notifications, clipboard, credential access, and audio capture.

Selecting “run again” forks a new run with a new operation revision and normal
permission checks. It never mutates the historical stream or silently resumes
an effect whose result was unknown.

Deterministic UI replay may reapply reducer actions, recorded state updates,
document steps, clock values, random seeds, and mocked capability results. The
UI must show when an event cannot be deterministically represented and stop at
the last valid checkpoint rather than fabricate state.

## Instrumentation boundary

Canvas-style compiler instrumentation may wrap known React `useState` setters
and `useReducer` dispatches and attach source callsites. That is useful trace
data but not a complete React state model. Application code can hold state in
external stores, closures, DOM state, browser APIs, and packages.

Therefore every generated view also receives a versioned runtime trace bridge
and explicit action reducer boundary. Instrumentation coverage is recorded in
the artifact manifest; unsupported state is marked opaque. Hyper Term does not
promise React Fiber or arbitrary V8 heap rewind.

## Local bug capsule

A user can export a bounded, inspectable bug capsule containing:

- source and document revisions or redacted patches;
- artifact manifest, output digests, source maps, compiler trace, and lock hash;
- relevant domain and UI events plus checkpoints;
- recorded tool/effect metadata and explicitly selected payloads;
- terminal screen or transcript ranges and Computer Use evidence references;
- Deno, compiler, WebView, OS, architecture, and component-schema versions;
- reproduction instructions that default to replay-only mode.

Secrets, environment values, provider prompts, terminal output, screenshots,
and MCP payloads are excluded or redacted by default. Export previews show the
exact included data.

## Consequences

This model can answer “what happened?” and reproduce UI bugs without pretending
the real world is reversible. It requires stable event schemas, migration,
checkpoint compatibility, content retention, and deterministic projection
tests. Event volume must be controlled through batching and artifact references.

## Offline bug capsule evidence (2026-07-20)

The authenticated ACP Workbench can now request a Rust-generated, replay-only
Bug Capsule for the exact current Artifact. Rust joins the accepted Artifact,
editor checkpoint, runtime projection, and pinned compiler environment behind
the existing task and Artifact fences. The response is bounded to 512 KiB and
contains an integrity digest that survives JSON save, reopen, and offline
verification.

Accepted and unpublished source, bundle, CSS, and source-map bodies are excluded
by default. Their virtual paths, byte counts, modification state, and SHA-256
digests remain available for matching on the originating machine. Deterministic
runtime inputs retain the Rust store's sensitive-key redaction. Console and
error observations retain only sequence-preserving excluded placeholders.
Terminal output, provider prompts, MCP payloads, environment values, Computer
Use frames, and screenshots have explicit excluded inventory rows.

The Time Travel tab first shows a compact export action. After preparation it
shows the exact included, digest-only, and excluded inventory before enabling a
user-initiated JSON download. The WebView verifies the capsule digest but cannot
add files or other workspace data to the Rust response. Rust unit and gateway
tests cover source and observation exclusion, sensitive runtime redaction,
modified-editor digests, size limits, tamper detection, and offline parse plus
digest verification.

The desktop supervisor now also accepts `--bug-capsule PATH`. Rust requires an
absolute, regular, non-symlink file bounded to 512 KiB, parses it without
following links, and revalidates the complete contract. In addition to the
capsule digest, import recomputes the deterministic replay projection digest;
an attacker cannot replace semantic event identity and merely sign the changed
JSON with a new outer digest.

After validation, the Agent gateway exposes exactly one read-only
`/agent/debug-capsule` projection behind the per-launch desktop token. This
route needs no ACP session ID because opening a capsule must not start or depend
on a provider. Native creates a dedicated `Capsule` tab and the packaged
Workbench renders metadata, the exact inventory, and a prefix cursor over
action, checkpoint, and effect-receipt events. No live effects, Agent prompt,
shell, MCP, Computer Use, source body, or arbitrary file-open capability is
available in this mode.

## Implementation evidence (2026-07-19)

The first durable Artifact-timeline slice now uses the existing Rust authority
instead of the former React-only trace list. Every accepted GenUI revision was
already an `ArtifactAccepted` event in the fsynced JSONL journal, while its
bounded virtual source tree, compiled output, CSS, and source map were stored in
the daemon's private Artifact store. The daemon now projects the latest 64
accepted revisions for one task, including journal sequence, timestamp,
operation identity, compiler identity, and content digest.

Both history metadata and historical source require the authenticated Agent
session plus the exact current Artifact ID as a fence. The requested historical
Artifact must also belong to that task's journal. Advancing the task invalidates
an already-open stale Workbench URL. A restart test accepts two revisions,
reopens the daemon, reproduces the same ordered projection, and reads the first
revision's exact source. A gateway test covers current-fence rejection,
authentication, bounded metadata, and historical source recovery.

The Workbench Time Travel tab renders this Rust-owned timeline. “Load as draft”
fetches the exact old source, requires the current fixed virtual path set, marks
changed files, opens CodeMirror Diff, and runs only the network-closed advisory
preview compiler. It never replays Shell, filesystem, ACP, MCP, Computer Use,
clipboard, notification, or audio effects. Publishing the restored draft still
creates a new revision and passes the normal approval and pinned-Deno path. A
480-pixel browser flow proves two-file history restore, current-versus-history
Diff, live preview reload, enabled publish, and zero page overflow.

## Runtime checkpoint evidence (2026-07-20)

The next slice adds an explicit generated-runtime boundary rather than
instrumenting React internals. `@hyper/runtime` exposes bounded `traceAction`
and `traceCheckpoint` calls. The network-closed iframe assigns a new stream ID
to every accepted local render and sends semantic events to its trusted parent.
The parent validates the exact iframe, channel token, local preview identity,
event schema, and byte bounds. It forwards events only while the editor source
is byte-for-byte equal to the Rust-accepted source; events from unpublished
drafts remain visible only in the ephemeral compiler trace.

The authenticated Rust gateway then fences the request to the current task,
Artifact ID, and source revision. It redacts sensitive keys, enforces depth,
node, string, event, batch, and journal limits, assigns the canonical event
sequence and SHA-256 payload digest, and appends mode-0600 JSONL evidence. Exact
client retries are idempotent. Gaps, conflicting retries, stale revisions,
symlinks, oversized events, digest tampering, and torn tails are rejected. The
Workbench batches events briefly and renders the durable projection in Time
Travel without exposing a replay action.

Validation is deliberately split at the authority boundary. Rust store and
gateway tests prove authentication, current-revision fencing, redaction,
ordering, restart recovery, and tamper rejection. Browser verification proves
the isolated preview emits action/checkpoint events, unpublished-draft events
stay labelled as non-durable, the Time Travel sections render without console
or page errors, and both 1440-pixel and 480-pixel layouts avoid page-level
horizontal overflow.

## Deterministic reducer replay evidence (2026-07-20)

The explicit runtime boundary now exports `useReplayReducer` and
`replayableEffect`. Live reducer dispatch records a bounded semantic action.
Generated code may also record an explicit state checkpoint. A replay session
rebuilds that named reducer from the ordered checkpoint/action range and makes
its dispatch inert, so interacting with the historical view cannot branch the
recorded state accidentally.

`replayableEffect` records one strict success or failure receipt containing the
JSON input and output or bounded error. Rust rejects ambiguous receipt shapes,
redacts them with the same sensitive-key policy as every runtime event, and
includes actions, checkpoints, and receipts in a restart-stable projection
digest. Console/error observations and wall-clock timestamps are deliberately
outside that digest. Payloads remain content-addressed by their individually
verified event digests.

The Workbench exposes “Replay to here” only for deterministic boundaries and
only while the editor remains byte-equal to the Rust-accepted source. The
isolated iframe verifies the full Rust projection digest before importing a
fresh bundle. During replay, `replayableEffect` has no path to its live callback:
it consumes the next exact-name, exact-input receipt in causal order and fails
closed for missing, reordered, failed, or redacted evidence. A browser-visible
flow proves that replaying through an action changes the rendered reducer state
and reports `effects substituted` without a page or console error.

This is still an explicit-runtime replay contract, not arbitrary React Fiber or
browser-state rewind. Durable editor transactions and accepted-source history
remain separate projections. The offline capsule joins bounded snapshots of
those projections for inspection; schema migration and cross-version replay
compatibility remain future work.

## Runtime journal migration evidence (2026-07-20)

The private runtime trace journal now writes storage schema version 2 without
changing the preview input schema or the deterministic replay projection. Each
stored row carries a SHA-256 over its task and Artifact context, ordered event
identity, sanitized payload, payload digest, redaction flag, and recorded time.
This closes integrity gaps that the replay-oriented payload digest deliberately
does not cover, while keeping wall-clock data outside canonical replay state.

On first read, Rust validates every version 1 JSONL event, computes the new
stored-event digest, and atomically replaces the bounded journal with fsync on
the file and parent directory. A crash before rename leaves version 1 intact;
after rename only the complete version 2 journal is visible. Tests prove that
the projection digest is identical before and after migration, exact retries
remain idempotent, timestamp-only and payload tampering fail closed, and unknown
future storage schemas are not guessed. Cross-version Bug Capsule migration is
still separate because its signed export digest covers a different contract.

## Validation gates

- Replaying the same checkpoint and event range must produce the same canonical
  domain, document, and reducer projection digest across process restarts. DOM,
  layout, and pixel output are validated separately and are not canonical state.
- Randomized chunking, reconnect, and duplicate-delivery tests must not change
  reducer output.
- Replay mode must prove that no shell, filesystem, network, MCP, ACP, Computer
  Use, clipboard, audio, or notification effect is invoked.
- A forked re-execution must have a new run and operation revision and pass the
  current permission policy.
- Source-mapped trace events must navigate to the original source and artifact
  revision, including after later edits.
- Bug-capsule tests must verify redaction, size limits, integrity, offline
  opening, and an explicit inventory of every included sensitive artifact.
