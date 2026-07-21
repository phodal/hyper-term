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
renderer. Toggling hunks changes only local review state; submitting the chosen
set creates one operation proposal. The permission broker verifies review
digest, base revision, path, selected hunks, worktree, and current file state
before applying anything.

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
preview, source-mapped runtime diagnostics, and Rust-journaled accepted-
Artifact Time Travel. It still runs against an inert demo broker when opened by
itself, so that browser surface is not evidence of workspace-write authority.

The Rust acceptance path now retains the exact bounded virtual source snapshot
for every new GenUI artifact and exposes only the current task artifact through
an authenticated, no-store Agent endpoint. That closes the missing source
recovery seam needed to open Agent-generated code in the trusted editor without
letting the WebView read the workspace. The Native host now keeps Agent tabs
single-pane by default and mounts the packaged Workbench on the right only when
an ACP session has a current artifact. The Workbench loads that exact source,
then offers draft CodeMirror, Diff, Time Travel, and isolated local preview
surfaces. CodeMirror now obtains lint diagnostics and completion from a
persistent Rust-supervised Deno LSP session over an authenticated, ACP-only
artifact endpoint. Rust creates a private source snapshot, fences every request
to the current artifact and source revision, and returns only normalized bounded
advisory data. A real Deno integration test covers `didOpen`, `didChange`,
diagnostics, and completion; a 360-pixel browser pass proves the editor, LSP
status, and preview headers do not overflow.

Workbench edits can now enter a separate Artifact publish transaction. The
trusted editor submits the complete bounded virtual source tree plus its exact
base artifact and source revision. Rust rejects stale revisions, changed path
sets, overlapping drafts, and non-ACP sessions before creating a
`hyper_term.genui.compile` Approval Block. `AllowOnce` moves that exact
operation through dispatch, recompiles with the digest-pinned Deno and
`esbuild-wasm` runtime, accepts a new immutable Artifact revision, records its
receipt, and refreshes the Workbench from Rust source. The browser's 260 ms
compiler remains advisory local preview only. Publishing an Artifact still
does not write to the workspace. Instead, the accepted Artifact can now enter a
second, independent `hyper_term.workspace.apply` transaction. The user maps a
bounded set of one to 32 Artifact source paths to unique workspace-relative
targets. Rust normalizes and sorts the set, then captures the exact Artifact
source revision plus every target parent identity, existing file identity,
mode, content digest, and bounded UTF-8 contents before producing one review.
The renderer receives the immutable before/after set for grouped, read-only
CodeMirror diffs but has no file API. The first phase creates no operation:
Rust computes a bounded Patience line diff with stable digest-derived hunk IDs,
returns at most 256 hunks per file, and binds the complete review to the exact
Artifact and captured workspace bases. The Workbench selects those immutable
hunk IDs. Only its explicit “Create approval” action sends the review digest
and full per-file selection back to Rust.

Rust recomputes the review, rejects stale, unknown, or duplicate hunk IDs, and
reconstructs the selected UTF-8 contents itself. That selected set is projected
as one `FileEdit / WorkspaceWrite` operation with a canonical transaction digest
and remains unchanged until the matching Approval Block receives `AllowOnce`.
Dispatch rechecks the current Artifact and every workspace base. Traversal,
duplicate targets, VCS metadata, symlink parents, special files, files over 1
MiB, changed inodes, changed modes, and changed digests fail closed before
installation. The Rust executor then stages the selected set in already-open
parent directories, keeps private backups of existing files, installs each
target atomically, verifies the results, and rolls back already-installed
members if a later member fails. Before staging, Rust atomically fsyncs a
bounded mode-0600 manifest under the private gateway state directory. The
manifest advances through `preparing`, `prepared`, `rolling_back`, `committed`,
or `rolled_back` and records identities and digests, never source content. It
remains after a terminal filesystem result until the daemon has journaled the
matching operation receipt; only then is it acknowledged and removed.

On restart, the gateway classifies every target as the exact reviewed base, the
exact staged proposal, or unknown. A prepared set that is entirely proposed is
committed; a base/proposed partial set is rolled back; an interrupted rollback
continues idempotently. Unknown identities are never overwritten and block only
new Workspace Apply proposals while ordinary Terminal and Agent work remains
available. The daemon's restart-time `UnknownExecution` operation is resolved
to `Succeeded` or `Failed` only after that filesystem recovery. Tests inject
crashes during preparation, partial install, complete install before the commit
marker, and rollback, plus an external-writer conflict and daemon restart. A
gateway integration test also proves that preview creates no write, one exact
approval can apply one of two distant App hunks together with a second file,
and the unselected App hunk remains at its captured base. Unit tests cover
stable hunk IDs, byte-exact reconstruction including CRLF and no-final-newline
cases, invalid selections, successful two-file apply, stale-member preflight,
later-member rollback, no-replace creation, cleanup, and symlink escape
rejection. Browser passes at 760 and 480 pixels cover hunk toggles, responsive
Diff layout, action visibility, and zero horizontal page overflow.

The editor no longer collapses an Artifact to its entrypoint. One Studio owns
the complete Rust-returned virtual file map, keeps per-file CodeMirror drafts
alive across tab switches and workspace-review overlays, marks changed paths,
and binds Deno LSP to the selected document. The revision-fenced live build
snapshots all files and the declared entrypoint, so editing an imported module immediately
updates the isolated preview. Publishing sends the complete fixed path set;
local additions or removals are rejected before the request. A 480-pixel
browser flow proves a two-file relative import, per-file LSP readiness, draft
retention across file and review switches, and explicit multi-file Workspace
Apply mapping without page overflow.

The editor LSP consumes that same complete in-memory draft rather than only
the selected document. Every diagnostics or completion request carries the
fixed virtual file inventory, bounded to 1,000 files and 1 MiB including virtual
path bytes. Rust rejects
missing, additional, invalid, or stale paths before the private Deno session is
touched. The session synchronizes imported files before the selected document;
when any dependency changes it advances the selected document version and
invalidates cached diagnostics. A real Deno test changes an imported module
without changing the active file and observes the resulting cross-file type
error, while the authenticated gateway test proves incomplete snapshots fail
closed. Live build, preview, and LSP therefore see one draft revision instead
of disagreeing about unsaved imports.

Time Travel no longer depends on the lifetime of that mounted React tree. The
Workbench requests a bounded newest-first Artifact projection from the Rust
journal and lazily fetches the exact source of a selected historical revision.
Loading history changes only the local draft, preserves the current Artifact as
the Diff base, and sends the complete source tree through the advisory live
preview. The old source becomes authoritative only if the user publishes it as
a new approved Artifact revision. Daemon-restart and authenticated-gateway
tests cover persistence and task/current-revision fencing; a 480-pixel browser
flow covers restore, per-file dirty state, Diff, preview reload, and overflow.

A real Deno integration test separately covers Artifact approval, compilation,
replacement, source recovery, and stale revision rejection. Durable editor-
transaction/selection journaling, reducer trace checkpoints, and arbitrary
binary Artifact editing remain open. Tier 2 result acceptance has its own
bounded binary transaction path and does not make the WebView editor a binary
file authority.

## Durable editor migration evidence (2026-07-20)

The Rust-owned Artifact editor store now writes schema version 2. Every snapshot
and transaction carries a SHA-256 binding over the accepted source revision,
entrypoint, and complete ordered baseline file map. This prevents a retained
draft from being resumed against different accepted bytes merely because the
revision number and virtual paths happen to match.

Opening a version 1 store performs an in-place, bounded migration. Rust validates
the legacy snapshot and ordered journal against the current accepted Artifact,
replays each revision, writes one fsynced version 2 snapshot, and then atomically
replaces the absorbed journal with an empty file. If the process stops between
those last two steps, the version 2 snapshot safely dominates the still-present
older transactions on the next load. A mismatched version 2 baseline and an
unknown future schema both fail closed. Regression tests cover legacy snapshot
plus journal migration, exact reopen after migration, baseline substitution,
future schemas, compaction, torn tails, stale revisions, and fixed file sets.

Accepted Artifact source, runtime trace, and Bug Capsule upgrades are now also
implemented as separate version 2 contracts. Each preserves its own source,
storage, or replay digest rather than inheriting the editor format.

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
