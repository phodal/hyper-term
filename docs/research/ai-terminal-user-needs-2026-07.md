# What should a next-generation AI Terminal be?

- Date: 2026-07-18
- Audience: developers who regularly use Bash and coding agents
- Horizon: the next one to two years
- Scope: task authoring, voice, Bash history, shared control, attention,
  review, recovery, and multi-agent work

This is a fast public-signal study, not a quantitative user survey. Evidence
comes from official product documentation, repeated GitHub issue clusters, and
independent community workarounds. Frequency labels describe repeated signals,
not measured market incidence.

## Executive read

The opportunity is not a terminal with a chat sidebar. It is a local-first,
interruptible, auditable human–AI execution environment. The terminal remains
valuable because it is the universal last mile to developer tools, remote
machines, and composable automation, but its traditional primitives are too
weak for delegated work: one input stream, linear scrollback, process-oriented
tabs, and command-only history. Users repeatedly lose time because an agent is
waiting unnoticed, cannot reliably recover why work happened, and cannot tell
which actions were performed by a person versus an agent. Voice helps with
high-bandwidth intent and quick steering, but it is an input modality—not an
execution authority and not the product's organizing principle. The strongest
product wedge is an Agent Control Tower; the durable moat is an Operation
Ledger that links intent, actor, authority, effects, and evidence.

## Product thesis

> A next-generation AI Terminal compiles human intent into controlled tasks,
> supervises AI access to Shell and Computer surfaces, and preserves a causal
> record of every action, effect, decision, and verification.

Its primary model is no longer:

```text
tab → pane → process → scrollback
```

It becomes:

```text
Intent → Task → Operation → Effect → Evidence
```

Terminal, structured Agent, Browser, Computer Use, MCP, and dedicated APIs are
executors within that model.

## Why start from the Terminal?

Terminal has five properties worth preserving:

- almost every developer tool already has a CLI;
- text commands are composable, scriptable, and remote-friendly;
- SSH makes the same interaction model work across machines;
- commands can express exact, inspectable operations;
- expert users can always drop below an abstraction when it fails.

Its existing information model is the problem:

- a pane represents a process, not a durable user task;
- scrollback records rendered characters, not causal state;
- Bash history usually records a command, not its output or intent;
- terminal notifications know that something rang, not why a person is needed;
- human and agent input lack explicit ownership;
- ten parallel agents become ten panes the user must poll.

The product should retain Terminal as the escape hatch and compatibility layer,
then add task, control, memory, and review above it.

## The user's real jobs

### 1. Express an outcome quickly

The user wants to combine natural language, voice, selected files, symbols,
terminal blocks, screenshots, diffs, and browser state into a coherent brief.
They do not want to repeatedly explain which object “that error” refers to.

### 2. Convert a fuzzy request into a safe delegation

A useful task contract contains goal, scope, constraints, definition of done,
attached context, capability lease, workspace, and attention policy. The user
should be able to edit this contract before the agent acts.

### 3. Leave without babysitting

The agent should continue in the background. The user is interrupted only for
a question, approval, conflict, failure, takeover, or review—not for token
deltas or ordinary progress.

### 4. Steer or take over precisely

`Steer`, `Pause`, `Interrupt`, `Take over`, and `Abort` are different actions.
`Ctrl+C` cannot represent all of them. Human and agent must never type into the
same PTY or GUI simultaneously.

### 5. Understand what happened

The user needs to answer: who proposed this command, why it was necessary, who
approved it, where it ran, what it changed, whether the model saw complete
output, and how the result was verified.

### 6. Review an outcome instead of rereading a conversation

The handoff should be organized by completion criteria, diff, tests, commands,
screenshots, failures, and uncertainty. A stopped agent is not necessarily a
completed task.

### 7. Recover and reuse work

The user wants to search by intent and outcome, branch from a checkpoint, reuse
a successful investigation on another branch, and distinguish human Bash from
AI Bash. Resume must disclose whether context, process, files, and external
side effects were actually restored.

### 8. Coordinate tasks, not panes

Multi-agent work needs dependencies, owners, worktrees, resource leases,
permissions, budgets, blocking reasons, and review state. A pane remains a
debugging view, not the orchestration model.

## Ranked UX problems

| Rank | Problem | Severity | Frequency signal | Confidence | Product move |
|---|---|---:|---:|---:|---|
| 1 | Agents wait for input or approval without being noticed | Critical | High | High | Semantic attention inbox with deep links |
| 2 | Session history exists but cannot be reliably found, trusted, or resumed | Critical | Medium-high | High | Validated ledger, repairable index, explicit integrity state |
| 3 | Bash history cannot explain actor, intent, authority, or side effects | Critical | Emerging-high | High | Operation Ledger shared by human and AI |
| 4 | Stopping an agent does not reliably stop or account for child processes | Critical | Medium-high | High | Job registry, process ownership, orphan reconciliation |
| 5 | Users review chat and truncated output instead of verified outcomes | High | High | High | Review bundle and model-view/output receipts |
| 6 | Many agents become tab and pane management | High | High | High | Task graph and shared-control actions |
| 7 | Typing long technical intent is slow and physically demanding | Medium-high | Medium | Medium-high | Recoverable voice brief in the composer |
| 8 | Notifications create noise when they lack task semantics | Medium-high | Medium | High | Attention policy, aggregation, focus gating, earcons |

Repeated community reports describe agents sitting idle on approvals while the
user works elsewhere, and multiple small tools now exist solely to track which
terminal needs attention. This is anecdotal but independently repeated across
different workflows:
[waiting for input](https://www.reddit.com/r/ClaudeCode/comments/1qxdmcw/no_notification_when_a_claude_code_session_is/),
[multi-session cockpit](https://www.reddit.com/r/ClaudeCode/comments/1rml6ll/claude_code_terminal_users_how_are_you_managing/),
and [Codex notification requests](https://github.com/openai/codex/issues/3962).

History and recovery issues also form duplicate clusters. Reports show JSONL
data remaining on disk while resume or UI indexing loses the usable chain:
[Claude resume context](https://github.com/anthropics/claude-code/issues/15837),
[Claude partial history](https://github.com/anthropics/claude-code/issues/24304),
and [Codex reproducible transcripts](https://github.com/openai/codex/issues/2765).

## Product pillar 1: Mission Composer

The composer is a multimodal task compiler, not a chat box and not a shell
heuristic.

```text
Goal
Scope
Constraints
Definition of done
Context references
Capability lease
Attention policy
```

It accepts text, voice, pasted data, files, symbols, terminal operations,
screenshots, and UI elements. Context is attached by reference with a visible
preview and retention policy.

The composer exposes explicit intents:

- **Ask** — discuss without action authority;
- **Run** — execute an exact Shell operation;
- **Drive** — operate an application;
- **Delegate** — start or steer a durable agent task.

Natural language may suggest a mode, but it must not silently turn a question
into execution.

## Voice: important modality, wrong product center

Claude Code's current implementation provides useful evidence for the right
boundary: speech is transcribed into the same editable prompt, can be mixed with
typing, and uses project and branch vocabulary hints. It does not inject audio
directly into the PTY: [Claude voice dictation](https://code.claude.com/docs/en/voice-dictation).

### High-value voice jobs

- narrate architecture context, a bug story, constraints, and acceptance tests;
- answer a blocking question or steer an agent away from a bad direction;
- request a short status recap while away from the keyboard;
- provide an accessibility path for RSI, arthritis, or visual impairment;
- combine spoken “why” with structured files, screenshots, and terminal blocks.

### Low-value or dangerous voice jobs

- dictating exact paths, regex, quoting, flags, Git refs, or secrets;
- directly approving deletion, deployment, publication, or permission changes;
- always-on listening in shared or noisy environments;
- reading raw terminal output, secrets, or long reasoning aloud;
- automatically sending a transcript before the user sees uncertain terms.

### Recommended voice model

1. **Quick Talk** — push-to-talk steering and short questions.
2. **Voice Brief** — longer speech becomes a structured, editable Task Contract.
3. **Local Control** — “pause”, “take over”, and “stop” are handled by the
   control kernel rather than delayed as model prompts.
4. **Audio Attention** — distinct, configurable sounds for question, approval,
   failure, review ready, and completion; optional short redacted TTS recap.

Voice drafts should journal audio chunks, provisional transcripts, corrections,
confidence, and context anchors. A failed transcription must not destroy a long
recording. Local and cloud recognizers should be replaceable; the UI must state
where audio is processed.

A mobile voice request in the Claude community describes precisely this job:
high-level steering away from the desk, with a transcript preview before send:
[Claude mobile voice proposal](https://github.com/anthropics/claude-code/issues/25115).

## Product pillar 2: Operation Ledger

Traditional history layers each preserve only part of the truth:

| Layer | Useful data | Missing for AI work |
|---|---|---|
| Bash history | command, sometimes timestamp | output, cwd, actor, intent, approval, effect |
| Atuin | command, cwd, duration, exit, host, session | output, tool lineage, file effects, context receipts |
| Terminal scrollback | displayed characters | stable boundaries, process state, causal identity |
| Warp Blocks | command plus output as an atomic block | cross-agent causality, authority, recovery, effects |
| Agent JSONL | provider-specific model/tool events | stable cross-provider model and integrity guarantees |

Atuin and Warp demonstrate that richer records are already valuable:
[Atuin recorded fields](https://docs.atuin.sh/cli/guide/basic-usage/) and
[Warp Blocks](https://docs.warp.dev/terminal/blocks). Tools such as Entire go
further by tying agent transcripts and checkpoints to Git history:
[Entire CLI](https://github.com/entireio/cli).

The AI Terminal should own an append-only, provider-neutral ledger:

```text
Task
└─ Run
   ├─ Turn
   ├─ Operation
   │  ├─ proposal and parent cause
   │  ├─ actor and executor
   │  ├─ permission request and decision
   │  ├─ process tree
   │  ├─ stdout/stderr artifacts
   │  ├─ observed effects
   │  └─ verification outcome
   ├─ ContextReceipt
   ├─ Checkpoint
   └─ AttentionEvent
```

An operation records task/run/turn IDs, human or agent actor, input source,
host, workspace, cwd, bounded environment changes, exact argv, policy revision,
process group, timestamps, exit/signal, output hashes, file/process/network
evidence, retry/replay lineage, and redaction/retention labels.

### Human-facing history views

- **Task story:** goal → decisions → actions → outcome;
- **operation detail:** exact command, output, effects, and approval;
- **model view:** exactly which ranges, summaries, and redactions entered model
  context;
- **branch view:** checkpoint and replay lineage;
- **multi-agent lanes:** actor, workspace, dependencies, and conflicts;
- **evidence bundle:** a portable handoff to another person or agent.

“Safe replay” must compare current preconditions—cwd, branch, file hashes,
dependencies, host, and permissions—and should default to a sandbox or temporary
worktree. Conversation rewind cannot claim to reverse Bash side effects; Claude
documents the same limitation for file changes made through shell commands:
[Claude checkpointing](https://code.claude.com/docs/en/checkpointing).

## Product pillar 3: Shared-control Attention OS

The primary workspace is a task control tower, not a grid of agent terminals.

Each task exposes:

```text
owner and parent task
current state and operation
workspace and resource leases
permissions and budget
progress evidence
blocking reason
next useful human action
```

Shared control has explicit semantics:

- `Steer`: queue a direction at the next safe boundary;
- `Pause`: stop scheduling new work after the current safe operation;
- `Interrupt`: signal the active operation;
- `Take over`: revoke agent input and grant the human the surface lease;
- `Abort`: terminate the task's owned process tree;
- `Resume`: re-observe state before continuing.

The Attention Inbox accepts only semantic states:

```text
NeedsInput
NeedsApproval
Conflict
UnknownExecution
Failed
ReviewReady
Completed
```

Every attention item answers who is waiting, why, what delay means, whether the
user must answer/approve/take over, the proposed next action, and its risk. A
successful completion can usually enter Review quietly; a sensitive approval or
resource conflict may interrupt immediately.

## Three possible product directions

| Direction | Description | Strength | Weakness |
|---|---|---|---|
| AI Shell | Better command blocks, completion, voice, and inline Agent | Familiar and fast to ship | Easy for existing terminals to copy; still session-centric |
| Agent Control Tower | Task graph, attention, approvals, takeover, multi-agent | Solves the loudest current pain | Needs structured driver coverage |
| Execution Memory | Causal operation ledger, recovery, replay, audit, context receipts | Durable differentiation and team value | Benefits emerge after enough history exists |

Recommendation: use **Agent Control Tower as the adoption wedge**, build
**Execution Memory as the product moat**, and retain **AI Shell as the expert
compatibility surface**.

## Anti-patterns

- adding a chat sidebar and calling it an AI Terminal;
- automatically guessing whether natural language is a shell command;
- sending voice transcription directly to execution;
- inferring approval or completion from ANSI, title text, silence, or pixels;
- treating raw scrollback as durable memory;
- displaying one agent per pane as the orchestration model;
- allowing human and agent to write one surface concurrently;
- offering only `Ctrl+C`, with no steer/pause/takeover semantics;
- using global auto-approve to hide permission fatigue;
- reporting task success from process exit alone;
- preserving Bash and Computer Use in unrelated histories;
- notifying every completion instead of managing attention budget.

## Opportunity map

### Validate now

- Prototype Mission Composer with text, recoverable voice draft, structured
  context references, and editable Task Contract.
- Map one structured Agent to `Running`, `NeedsInput`, `NeedsApproval`, and
  `ReviewReady`.
- Record Human and Agent Bash into the same Operation Ledger with actor and
  parent cause.
- Test distinct `Steer`, `Pause`, `Take over`, and `Abort` controls.
- Compare task-centric review with reading the original chat/scrollback.

### Build this quarter if validated

- durable `hyperd`, ledger integrity and repair, process ownership, and
  reconnect;
- worktree and resource leases for parallel agents;
- Computer Use actions in the same ledger and permission broker;
- context receipts, safe replay, evidence bundles, and cross-task search;
- focus-aware desktop/audio/mobile attention projections.

### Needs deeper research

- observe 6–10 developers running two or more agents for a week;
- measure time lost to unnoticed waiting, resume, and review;
- compare voice brief quality with typed prompts across English and
  Chinese-with-English technical vocabulary;
- test whether users understand and trust steer/pause/takeover distinctions;
- interview security/platform teams about retention, redaction, and audit needs;
- determine which parts of execution memory users will actually revisit.

## Source map and limitations

- Official docs contributed current product capabilities and protocol
  boundaries: Claude voice/checkpoints, Codex app-server, Warp Blocks, and Atuin
  history.
- GitHub issue clusters contributed failure and recovery evidence. Issue volume
  is biased toward users who encounter problems and should not be treated as
  usage prevalence.
- Reddit and HN contributed multi-session, accessibility, voice, and attention
  workarounds. These are anecdotal, but several independent tools solving the
  same waiting/session problem raise confidence in the underlying job.
- No internal support, telemetry, or interview data was available. Voice
  demand, team audit willingness, and willingness to change terminals remain
  the weakest commercial signals and require direct research.
