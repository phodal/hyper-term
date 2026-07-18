# ADR 0011: Project agent protocols into a versioned Block Render document

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0006](0006-semantic-time-travel-debug.md),
  [ADR 0009](0009-rust-acp-mcp-adapters.md)

## Context

ACP is a semantic protocol between a client and an agent, not a portable UI
tree. Its stable version 1 streams user, agent, and thought content; tool-call
lifecycle and content; plans; commands; modes; configuration; session metadata;
and usage. ACP `ContentBlock` covers text, image, audio, resource link, and
embedded resource. Tool-call content adds diff and terminal references.

This decision was checked against the official ACP repository at commit
[`442101dc8f0d53ba94832996e3f9297da0a6acae`](https://github.com/agentclientprotocol/agent-client-protocol/tree/442101dc8f0d53ba94832996e3f9297da0a6acae),
which declares protocol version 1 as the current stable version.

These types say what happened, but not how a terminal client should compose,
virtualize, focus, persist, or safely render it. MCP content, local PTY output,
Computer Use evidence, approvals, generated UI artifacts, and editor documents
also need to appear in the same task surface without being forced into ACP
extensions.

Using protocol messages directly as React components or Native SDK widgets
would couple the durable product model to one protocol version and one renderer.
It would also make a renderer restart, protocol upgrade, or WebView failure a
history problem.

## Decision

The Rust control kernel will project canonical domain events into a versioned,
renderer-independent `BlockDocument`. ACP, MCP, PTY, Computer Use, and compiler
adapters remain inputs to this projection; none is the render model itself.

Conceptually:

```text
ACP / MCP / PTY / Computer Use / compiler events
  -> Rust normalization and operation journal
  -> BlockProjector
  -> BlockSnapshot + ordered BlockPatch stream
  -> renderer registry
       -> native block renderer
       -> terminal surface renderer
       -> trusted WebView editor
       -> isolated WebView artifact
       -> deterministic fallback renderer
```

The Block document is a read model. Canonical domain events remain the source of
truth. Block snapshots may be persisted as disposable checkpoints for fast
resume, but rebuilding them from the journal must produce the same semantic
digest.

## Block envelope

Every block has stable identity, revision, lifecycle, source, and actions:

```text
BlockEnvelope
  schema_version
  block_id / block_revision / document_revision
  parent_block_id / order_key
  task_id / run_id / operation_id
  source_kind / source_id / causation_id
  kind / render_slot / trust_class
  lifecycle / reported_status / attention
  payload_ref / payload_digest
  declared_actions[] / presentation_hints[]

BlockLifecycle
  Draft | Queued | Running | Waiting | Succeeded
  Failed | Cancelled | UnknownExecution

BlockAction
  action_id / input_schema / risk_class
  required_capabilities[] / expected_block_revision
```

`block_id` identifies a logical object such as a message, tool call, plan, or
terminal across updates. `block_revision` increases whenever its semantic
content or lifecycle changes. A block's `kind` is immutable for a given
`block_id`; changing kind creates a new block. Renderer-local focus, hover,
selection, measured height, WebView lease, GPU generation, and animation state
are not placed in the block payload.

`reported_status` preserves the state reported by an external protocol. It is
not the Rust operation state. For example, ACP tool-call `pending` does not mean
that Hyper Term is waiting for human approval. Approval follows its own
`Proposed -> PolicyCheck -> WaitingHuman -> Authorized -> Dispatching ->
Succeeded | Failed | Cancelled | UnknownExecution` operation state machine.

Presentation hints are untrusted preferences. The host chooses a renderer from
its measured capabilities, policy, accessibility mode, and current platform.

## First block vocabulary

The initial registry contains a deliberately bounded vocabulary:

| Block kind | Purpose | Default projection |
| --- | --- | --- |
| `MessageBlock` | User, agent, or system message with ordered text/image/audio/resource parts. | Native Markdown and media blocks. |
| `ThoughtBlock` | Collapsible agent reasoning or progress narration where policy permits display. | Native, collapsed by default. |
| `ToolBlock` | Tool identity, arguments summary, lifecycle, nested results, verification, and errors. | Native shell with child blocks. |
| `TerminalBlock` | Stable terminal reference, command metadata, exit state, and transcript/screen handles. | Dedicated terminal surface. |
| `DiffBlock` | File identity, base/result revisions, hunks, acceptance state, and diagnostics. | Native summary; WebView editor on expansion. |
| `PlanBlock` | Ordered steps and their status. | Native stepper/timeline. |
| `ApprovalBlock` | Exact proposed effect, scope, risk, revision, and available decisions. | Native, always trusted chrome. |
| `ElicitationBlock` | Restricted form schema or out-of-band URL request. | Native form for supported schema; safe fallback otherwise. |
| `ArtifactBlock` | Accepted diagram, table, debugger, Canvas, or Agentic UI artifact by content ID. | Native registry component or isolated WebView. |
| `ComputerUseBlock` | Observation, screenshot reference, target application, proposed input, and receipt. | Native shell with image/overlay surface. |
| `ResourceBlock` | File, URI, image, audio, or embedded resource metadata and bounded content reference. | MIME-aware native renderer or download/open affordance. |
| `DiagnosticBlock` | Unsupported protocol item, malformed content, renderer failure, or compatibility warning. | Deterministic native text fallback. |

Containers may nest blocks, but arbitrary layout instructions are not part of
the first schema. The client owns spacing, typography, virtualization, focus,
and responsive layout. Agent-authored layout belongs to an accepted ADR 0004 UI
artifact, not to ordinary ACP content.

## ACP projection

The stable ACP v1 mapping is explicit:

| ACP update | Block operation |
| --- | --- |
| `user_message_chunk` / `agent_message_chunk` | Append a content part or delta to one stable `MessageBlock`; use `messageId` when present and a turn-local stream identity otherwise. |
| `agent_thought_chunk` | Append to one policy-filtered `ThoughtBlock`. |
| `tool_call` / `tool_call_update` | Upsert one `ToolBlock` by `toolCallId`; replace patch fields using ACP semantics. |
| tool `content` | Replace the normalized nested content collection under the owning `ToolBlock`; preserve stable child IDs for semantically unchanged items. |
| tool `locations` | Replace the complete normalized location collection; it is not an append delta. |
| tool `diff` | Upsert a nested `DiffBlock`. |
| tool `terminal` | Attach a `TerminalBlock` keyed by the ACP terminal reference; the reference cannot create, adopt, or take authority over a PTY. |
| `plan` | Replace or patch the active `PlanBlock`. |
| commands, modes, config, session info, usage | Update session chrome projections; create transcript blocks only when the event is user-relevant history. |

The projector also handles ACP messages outside `SessionUpdate`:

- `session/request_permission` creates or revises an `ApprovalBlock` and links
  it to a Rust operation; it never grants permission by itself;
- the `session/prompt` response `StopReason` creates a `TurnCompleted` journal
  fact and updates the session/turn projection without inventing a chat message.

External identifiers are namespaced by session, turn, role, and source. An ACP
`messageId` is never accepted as a globally unique internal block ID. When it is
absent, the adapter creates a turn-local stream identity and records that the
boundary was inferred. Repeated identical text chunks are not deduplicated by
payload digest because both chunks may be valid content; transport sequence and
adapter idempotency keys decide duplicate delivery.

Future ACP versions are normalized by a version-specific adapter before they
reach `BlockProjector`. Unknown fields survive as bounded evidence attachments,
not renderer instructions. The current ACP repository and negotiated protocol
version are pinned independently, as required by ADR 0009.

Unknown variants project to `DiagnosticBlock` and remain bounded, redacted
evidence. ACP `_meta`, MIME labels, annotations, titles, and presentation hints
cannot dynamically register a renderer or grant a capability.

Hyper Term extensions such as `ArtifactBlock` use the internal event schema.
When they must cross ACP or MCP, they use an ordinary resource with an explicit
MIME type, content digest, and text fallback. The client does not redefine ACP
`ContentBlock` globally.

## Snapshot and patch protocol

A renderer starts from a bounded snapshot and consumes ordered patches:

```text
BlockSnapshot(document_revision, blocks[], renderer_capabilities)

BlockPatch
  stream_sequence / base_revision / target_revision
  operations[]

BlockOperation
  Insert | Upsert | AppendContent | ReplacePayload
  Move | Remove | SetLifecycle | SetAttention
```

Patches are idempotent by sequence and block revision. A gap, stale base, or
renderer restart requests a fresh snapshot. Token streaming is coalesced at a
frame-sized cadence; a token never causes a complete document serialization.

High-volume data stays on dedicated planes:

- PTY bytes, terminal cell-grid snapshots, and damage records use an ordered
  terminal channel; `TerminalBlock` carries only identity and presentation state;
- large images, source maps, diffs, transcripts, and compiled artifacts are
  content-addressed resources referenced by digest;
- renderer control patches never carry arbitrary multi-megabyte payloads.

## Renderer registry

Renderers implement the semantic equivalent of:

```text
supports(block_kind, payload_schema, platform_capabilities) -> score
mount(block_snapshot)
apply(block_patch)
capture_ephemeral_state() / restore_ephemeral_state()
unmount()
```

The reference routing policy is:

- native blocks for transcript chrome, Markdown, plans, tool lifecycle,
  approvals, elicitation, attention, and bounded media;
- a terminal-specific renderer for the cell grid, selection, IME, and scrollback;
- trusted WebView islands for Tiptap, CodeMirror/Monaco, complex diff review,
  and other reviewed workbench tools;
- bridge-free isolated WebViews for accepted Agentic UI artifacts;
- a native text/JSON diagnostic fallback for every unsupported type.

Native SDK `.native` markup is application code compiled ahead of time. Agents
cannot supply or hot-load arbitrary `.native` markup in a release. The native
renderer is a fixed registry that maps validated block payloads to precompiled
widgets. Agent-generated React remains an ADR 0004 artifact.

## Virtualization and WebView islands

The transcript is one variable-height virtual block list, not one native view or
WebView per historical item. Stable block IDs become renderer keys so scroll,
selection, expansion, and focus survive updates and replay.

WebViews are scarce process/platform surfaces. The host maintains a low,
capability-profiled pool for visible or selected editor/artifact blocks. An
off-screen WebView block is suspended and represented by a native placeholder,
thumbnail, or last-known-good snapshot. Its durable document or artifact state
is restored when remounted. Pool eviction never discards Rust-owned state.

Pools are separated by trust profile, not just by renderer type:

- one trusted-workbench surface normally hosts editor and large-diff tabs;
- isolated artifact surfaces may be reused only inside the same sandbox,
  storage, bridge, and network profile;
- remote-content surfaces are origin-partitioned and are destroyed when the
  platform cannot prove a complete reset;
- authentication, credential, and permission surfaces are ephemeral and never
  pooled.

Every WebView-to-host message includes `surface_id`, `renderer_generation`,
`lease_token`, `block_id`, and `block_revision`. Messages from an expired lease,
old generation, wrong trust profile, or stale block revision are rejected. A
reset revokes resource URLs, workers, timers, listeners, storage access, bridge
capabilities, and origin policy before the surface can be leased again; if that
cannot be verified, the surface is destroyed.

Focus uses an explicit lease. Only one interactive block owns keyboard/IME input
at a time; terminal, editor, preview, voice composer, and global shortcuts
cannot all consume the same event.

## Actions and authority

Blocks contain action declarations, not executable callbacks. Every interaction
returns a typed intent:

```text
UiIntent
  block_id / expected_block_revision
  action_id / input_payload
  actor / renderer_instance / interaction_id
```

Rust rejects stale revisions, validates the action schema and capabilities, and
creates an immutable operation revision before any effect. Native or WebView
renderers cannot launch commands, write files, call MCP/ACP tools, or perform
Computer Use directly.

Untrusted ArtifactBlocks receive no native bridge. They can only send the
schema-bounded `ProposeAction` and trace messages from ADR 0004. Remote content,
terminal escape sequences, Markdown, and protocol metadata never become action
declarations merely because they contain matching text.

## History and Time Travel

Time Travel replays the domain journal into `BlockProjector`; it does not replay
pixels or re-execute a renderer action. The canonical replay digest covers block
identity, order, semantic payload, lifecycle, and declared actions, but excludes
layout, WebView DOM, GPU output, and transient selection.

Renderer checkpoints may record bounded scroll anchors, expanded block IDs,
selection anchors, and focused block identity. Native SDK automation and visual
record/replay can validate that a renderer projects the same block document, but
it cannot replace the Rust operation journal.

## Consequences

ACP remains interoperable while Hyper Term gains a product-level render model
that can serve React, Native SDK, a daemon client, or tests. The same journal can
be projected with different renderers, and unsupported content degrades visibly.

The cost is another versioned schema, adapter fixtures, patch protocol, renderer
registry, and resource store. Native and WebView renderers must pass semantic
equivalence tests even when their pixels differ.

## Implementation evidence (2026-07-19)

Block schema version 2 adds the first real `ArtifactBlock` projection. The Block
contains the accepted ID, source revision, entrypoint, compiler identity, and
content digest; bundle, CSS, and source-map bytes remain in the Rust-owned
resource store. One stable task-derived block ID is upserted for each accepted
revision, so a candidate or compiler failure cannot displace the prior block.

The Native adapter accepts only the `artifact` kind with
`trust_class=isolated_artifact` and validated metadata. It converts the current
Agent session's Block into an authenticated local preview URL and leases a
dedicated system WebView aligned to the Agentic UI slot. Inactive terminal and
artifact WebViews collapse to a 1-by-1 inline surface. Projection tests, the
authenticated HTTP fixture, browser capsule verification, and Native semantic
automation cover this initial route; pooled multi-block eviction and stale
renderer-lease tests remain open.

## Validation gates

- Golden ACP fixtures for every negotiated version produce the same canonical
  Block document regardless of transport chunking, reconnect, or duplicate
  delivery.
- ACP v1 fixtures cover all 11 `SessionUpdate` variants, five `ContentBlock`
  variants, three `ToolCallContent` variants, optional and absent `messageId`,
  permission requests, prompt stop reasons, and unknown variants.
- Fixtures prove that repeated equal text chunks remain repeated, while tool
  `content` and `locations` use complete-collection replacement semantics.
- Randomized snapshot/patch loss, reordering, and restart tests either converge
  to the same digest or request a new snapshot; they never silently diverge.
- A transcript with 100,000 mixed-height blocks stays within declared renderer
  node, memory, and frame-time budgets while preserving stable IDs and anchors.
- No test creates one WebView per block; pool limits, eviction, restoration,
  cross-profile isolation, stale-lease rejection, focus transfer, and bridge
  policy are explicit and observable.
- PTY bursts and model streaming remain ordered and bounded without serializing
  the complete Block document or passing bulk data through a JSON command bridge.
- Native, trusted-WebView, isolated-WebView, and fallback renderers produce the
  same action IDs and lifecycle projection for shared fixtures.
- Replay mode renders approvals, tools, terminals, MCP/ACP, and Computer Use as
  observations and proves that no external effect is invoked.
