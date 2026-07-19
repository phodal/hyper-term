# ADR 0007: Use React projections with transactional editor adapters

- Status: proposed
- Date: 2026-07-18
- Depends on: [ADR 0004](0004-versioned-agentic-ui-artifacts.md),
  [ADR 0006](0006-semantic-time-travel-debug.md)

## Context

The workbench must support a durable multimodal composer, structured task and
artifact documents, code edits, diffs, generated UI previews, streaming agent
state, and debugging. No single editor library is the product model, and editor
state cannot be allowed to become an unversioned WebView-only island.

## Decision

React is the trusted workbench projection layer. Individual editors implement
versioned adapter contracts and emit transactions that Rust can journal and
checkpoint.

The proposed first-spike editor candidates are:

- **Tiptap/ProseMirror** for the composer, task plans, review notes, and mixed
  text/artifact documents. Persist schema-versioned JSON and ProseMirror steps,
  never generated HTML as the canonical document.
- **CodeMirror 6** for embedded code and patch editing. Its immutable state and
  transaction model align with Time Travel, and `@codemirror/merge` covers
  side-by-side and unified review.
- **Monaco** is an optional, lazy-loaded adapter for workflows that prove they
  need its full editor and diff surface. It is not paid as an idle startup and
  memory cost in every terminal window.
- **Deno LSP** provides TS/TSX diagnostics and completion through the supervised
  sidecar. LSP output is advisory editor data, not file or execution authority.

Tiptap describes a headless, schema-driven editor and supports JSON content;
CodeMirror documents a [functional state and transaction core](https://codemirror.net/docs/guide/#state-and-updates)
plus [merge views](https://codemirror.net/docs/ref/#merge.MergeView). Monaco
remains available for its [diff editor](https://microsoft.github.io/monaco-editor/).

## Adapter contract

Each editor adapter maps its native library events into a stable product model:

```text
EditorDocument
  document_id / schema_version / base_revision
  content_ref / selection / attachments[]

EditorTransaction
  transaction_id / actor / base_revision
  operations[] / selection_after / intent

EditorCheckpoint
  document_revision / canonical_content_digest
  adapter_kind / adapter_state_version
```

The adapter may keep ephemeral layout, composition, hover, and viewport state in
the WebView. Content, selections needed for resume, accepted edits, attachments,
and review decisions are sent as bounded transactions. IME composition is not
journaled as a sequence of half-formed commands; it commits as an editor edit.

## Diff and apply boundary

The visual diff is not the patch authority. Rust computes or validates a patch
against an exact base file digest and exposes immutable files and hunks to the
renderer. Accepting or rejecting a hunk submits an operation proposal. The
permission broker verifies base revision, path, selected hunks, worktree, and
current file state before applying anything.

CodeMirror or Monaco may calculate presentation-only inline differences, but a
renderer-produced diff never directly writes the workspace. A changed base
invalidates the approval and requests a rebase or refreshed review.

## Agentic interaction and voice

The composer retains explicit intent: Ask, Run, Drive, or Delegate. Text, code,
files, screenshots, terminal ranges, artifacts, and voice transcripts become
typed attachments to the draft rather than an uncontrolled context dump.

Voice is an input adapter, not an execution shortcut:

```text
OS audio permission -> AudioSource -> VAD -> streaming ASR
                    -> transcript draft -> human edit/confirm -> intent
```

Audio capture, transcription, and retention are visible states. A transcript
may edit the composer or answer a question, but it cannot bypass the selected
intent, action revision, or approval. Push-to-talk is the initial interaction;
always-on listening and wake-word behavior require a separate ADR.

Agent streaming updates are reduced and rendered in batches, not one React
state update per token. Terminal output stays in the terminal projection. Rich
artifacts are opened by stable reference and revision.

## Extension policy

- Trusted editor extensions are packaged, reviewed, and versioned with the host.
- An AI may generate document content, UI IR, or sandboxed TSX, but not install a
  Tiptap, CodeMirror, or Monaco extension into the trusted origin at runtime.
- Editor commands that imply native effects emit proposals through the same
  action bridge as Agentic UI.
- Accessibility semantics, focus transfer, keyboard takeover, IME, and CJK are
  host responsibilities and cannot be delegated to an arbitrary preview.

## Consequences

This candidate stack covers structured prose, lightweight code, and review while
keeping Monaco available for evidence-backed cases. Production selection remains
behind the gates below. Transaction normalization adds adapter work, but it gives
resume, replay, multi-surface consistency, and permission-safe apply. Tiptap and
CodeMirror version upgrades require schema and transaction migration tests.

## Implementation evidence (2026-07-19)

The Deno-built React Workbench contains a real CodeMirror 6 TSX editor,
`@codemirror/merge` diff view, persistent `esbuild-wasm` Worker, isolated live
preview, source-mapped runtime diagnostics, and bounded Time Travel trace. It
still runs against an inert demo broker when opened by itself, so that browser
surface is not yet evidence of workspace-write authority.

The Rust acceptance path now retains the exact bounded virtual source snapshot
for every new GenUI artifact and exposes only the current task artifact through
an authenticated, no-store Agent endpoint. That closes the missing source
recovery seam needed to open Agent-generated code in the trusted editor without
letting the WebView read the workspace. The Native host now keeps Agent tabs
single-pane by default and mounts the packaged Workbench on the right only when
an ACP session has a current artifact. The Workbench loads that exact source,
then offers draft CodeMirror, Diff, Time Travel, and isolated local preview
surfaces. Multi-file selection, transaction journaling, Deno LSP editor
requests, and brokered apply remain open.

## Validation gates

- Restore Tiptap and CodeMirror documents, selections, attachments, and undo
  boundaries after a WebView restart by either persisting versioned history
  state or deterministically rebuilding it from accepted transactions.
- Prove replay produces canonical document digests independent of transaction
  chunking.
- Test large files and diffs for typing latency, viewport memory, diff timeout,
  and lazy Monaco load cost.
- Compare Tiptap with a smaller ProseMirror integration and CodeMirror with
  lazy Monaco on startup, resident memory, IME/accessibility, transaction
  serialization, diff quality, and Time Travel reconstruction before acceptance.
- Verify stale-base hunk approval is rejected and no editor API can write files
  without a Rust operation.
- Test screen reader, keyboard-only use, IME, CJK, emoji, focus transfer, and
  sandbox-to-host navigation.
- Verify voice denial, cancellation, interruption, redaction, transcript edit,
  and “no execution from raw transcript” behavior.
