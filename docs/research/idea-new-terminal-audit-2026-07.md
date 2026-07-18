# IntelliJ IDEA New Terminal: product audit and architecture lessons

- Date: 2026-07-18
- Local build audited: IntelliJ IDEA 2026.1, `IU-261.22158.277`
- Active engine: Reworked 2025
- Flow: open the Terminal, run successful and failed commands, inspect completion
  and compatibility settings, run Codex interactively, and exit back to the shell
- Capture method: direct Computer Use of the installed macOS application

## Executive read

JetBrains' most useful contribution is not a block UI or an editor renderer. It
is a two-year product experiment that established the correct architecture
order for an intelligent terminal:

1. preserve a transparent PTY and native shell behavior;
2. derive optional semantic overlays from shell integration;
3. keep a raw terminal escape hatch;
4. add completion and AI affordances without replacing the shell;
5. treat an AI CLI as a compatible TUI unless a separate structured protocol is
   available.

The 2024 Experimental New Terminal took control of the input line, prompt,
completion, history, and command blocks. JetBrains later documented that this
broke shell shortcuts, user prompts, plugins, and TUI applications. The 2025
Reworked Terminal returned keystrokes, signals, and commands to the PTY/shell,
kept JediTerm for xterm/VT100 semantics, and used the IDE Editor only for
rendering. JetBrains' conclusion is explicit: new capabilities must be layered
on top of a transparent terminal rather than inserted into its input path.

For Hyper Term, this sharpens the product thesis:

> Build a transparent Terminal Data Plane and a separate AI Control Plane. An
> Operation Block is a projection of durable execution state, not a replacement
> for the shell and not the source of truth for agent state.

Primary references:

- [JetBrains Terminal: A New Architecture](https://blog.jetbrains.com/idea/2025/04/jetbrains-terminal-a-new-architecture/)
- [Reworked Terminal becomes the default in 2025.2](https://blog.jetbrains.com/platform/2025/07/the-reworked-terminal-becomes-the-default-in-2025-2/)
- [Current IntelliJ IDEA Terminal documentation](https://www.jetbrains.com/help/idea/terminal-emulator.html)
- [Current Terminal settings documentation](https://www.jetbrains.com/help/idea/settings-tools-terminal.html)
- [Reworked Terminal API announcement](https://platform.jetbrains.com/t/reworked-terminal-api-is-available-in-2025-3/3159)

## The three generations

| Generation | Input owner | Semantic UI | Result |
|---|---|---|---|
| Classic | Shell through PTY | Minimal | Compatible and predictable, but difficult to extend |
| Experimental 2024 / New Terminal | IDE editor intercepted input and replaced prompt behavior | Full blocks, filtered history, IDE completion, natural-language command generation | Valuable concepts, but unacceptable shell and TUI compatibility regressions |
| Reworked 2025 | Shell through a transparent PTY | Editor-rendered terminal, optional separators, completion, links, shell integration | Correct compatibility baseline and current default |

The old natural-language-to-command flow is now explicitly documented as a
deprecated Experimental 2024 feature. Its automatic language detection could
also mistake commands such as `check` or `copy` for prompts. That is strong
evidence for explicit `Shell`, `Ask`, and `Delegate` modes instead of heuristic
mode switching:
[deprecated terminal AI command generation](https://www.jetbrains.com/help/ai-assistant/generate-terminal-commands.html).

## Flow audit

### Step 1 — Open the Reworked Terminal: healthy

The terminal keeps the user's Zsh prompt, aliases, colors, and existing
scrollback. Commands are separated by thin horizontal rules rather than large
cards. This is a good compromise: boundaries become scannable without turning
the terminal into a low-density feed.

Strengths:

- shell identity is preserved rather than replaced by an IDE prompt;
- the terminal stays visually secondary to the editor while remaining useful;
- separators are subtle and can be disabled;
- existing shell output and interrupted commands remain readable.

Risks:

- the thin separators do not reveal actor, duration, exit code, effects, or why
  a command ran;
- the shortcut-conflict notification is unrelated to the current terminal
  task and partially covers the work area;
- red and green prompt/edge colors are easy to over-interpret without labels.

### Step 2 — Run success and failure commands: mixed

The following harmless probes were executed:

```sh
printf 'IDEA_TERMINAL_PROBE\n'; pwd
sh -c 'printf "failure sample\n" >&2; exit 7'
```

Command and output boundaries remain clear, and the native shell receives the
exact input. However, the failed command is not explained as an operation. The
screen does not make exit code `7`, stderr, duration, or downstream effects
obvious. A user can infer failure from styling and content, but another agent
cannot safely treat those pixels or ANSI bytes as structured truth.

For Hyper Term, separators should be the lowest-fidelity projection of an
`Operation` that can expand to show:

```text
actor + intent + target + cwd
approval + started/ended + exit/signal
stdout/stderr artifacts + effects + evidence
```

### Step 3 — Command completion: useful, but deliberately narrow

The live prompt still shows the user's native Zsh autosuggestion. The IDE's own
completion is a separate layer. In this installation it is configured to appear
automatically only for parameters, with `Ctrl+Space` as the explicit invocation
shortcut.

This split is correct. Native completion remains available even when IDE
completion is disabled or unsupported. The weakness is discoverability: the
shortcut conflicts with macOS on this machine, and users must understand the
difference between shell suggestions and IDE suggestions.

Hyper Term should expose completion providers as advisory edits with visible
provenance:

```text
shell history | command spec | filesystem | project index | AI
```

Accepting a suggestion may edit a draft, but it must never silently execute it.

### Step 4 — Engine escape hatch: healthy, transitional

Reworked 2025 is selected and Classic remains available. An engine-level
fallback was sensible during migration, but it is too coarse as a long-term AI
architecture. Users should not need to replace the whole terminal engine to
disable blocks, completion, AI hints, notifications, or shell integration.

Hyper Term should keep one compatibility core and independently negotiate or
toggle enhancements.

### Step 5 — Completion and visual settings: healthy, settings-heavy

The current settings expose the important separation clearly:

- Reworked 2025 versus Classic engine;
- optional IDE completion;
- automatic completion scope;
- separate invocation and insertion shortcuts;
- project start directory and environment;
- font, column width, and contrast controls.

The default `Only for parameters` choice is conservative. It reduces popup
noise and avoids competing with shell subcommand history. The cost is that a
capability users associate with an “intelligent terminal” can seem absent until
they inspect settings.

### Step 6 — Compatibility and accessibility controls: healthy

The most important settings are not decorative:

- minimum contrast ratio;
- optional command separators;
- audible bell;
- mouse reporting;
- terminal-owned versus IDE-owned shortcuts;
- shell integration;
- link detection;
- Option/Meta behavior.

This is evidence that terminal compatibility is a matrix, not a renderer
feature. Hyper Term needs the same explicit policy boundaries, but the common
AI-era choices should be expressed as capability profiles rather than a long
flat settings page.

Visible accessibility strengths include a configurable minimum contrast ratio
and keyboard behavior controls. Screenshot and accessibility-tree inspection
cannot establish screen-reader quality, focus order under all modes, IME/CJK
behavior, zoom reflow, or whether state changes are announced. Those require
separate VoiceOver, keyboard-only, high-contrast, IME, and double-width glyph
tests.

### Step 7 — Run Codex as an interactive CLI: terminal compatibility healthy,
agent semantics absent

Codex `v0.144.5` started successfully in the audited IDEA Terminal. Its bordered
status area, prompt, colors, hyperlinks, and interactive input rendered inside
the existing terminal session. IDEA 2026.1 specifically changed `Escape` to be
handled by the shell and added `Shift+Enter` for multi-line AI CLI prompts,
showing that agent TUIs are now a first-class compatibility workload:
[IntelliJ IDEA 2026.1 Terminal fixes](https://blog.jetbrains.com/idea/2026/03/whats-fixed-intellij-idea-2026-1/).

What IDEA still cannot know from this PTY alone:

- whether Codex is planning, working, waiting for input, or waiting for approval;
- which output is a durable artifact versus a redraw;
- which commands were proposed or executed by the agent;
- whether stopping the TUI stopped child processes;
- when the task is review-ready rather than merely idle;
- which model context included which terminal bytes.

The current Terminal documentation can launch Junie, Claude Code, or Codex as
agent sessions, but this remains primarily a CLI launcher. It does not turn
their terminal output into a shared agent protocol. The documented `AI Agents`
dropdown was not visible in the audited toolbar even though `codex` was
available on `PATH`; the session had to be started as a normal shell command.
That may be a local rollout, license, plugin, or configuration difference, so it
is evidence of discoverability/configuration uncertainty rather than proof that
the feature is generally absent.

### Step 8 — Exit Codex: scrollback preserved, task history absent

After `/quit`, the Codex screen remains readable in the normal scrollback and
the shell prompt returns. This is useful evidence, but it is not a durable task
record. The scrollback is bounded, can be trimmed, and cannot answer who did
what or restore the agent session.

The product must distinguish:

1. shell history — exact commands entered;
2. terminal transcript — bytes and screen state;
3. operation ledger — actor, intent, authority, effects, and evidence;
4. agent memory — what later turns should know.

IDEA primarily improves the first two. Hyper Term's product advantage should be
the last two.

## What to borrow

### 1. Transparent PTY as a non-negotiable data plane

When the user is in Shell mode, keystrokes, signals, resize events, alternate
screen behavior, and mouse reporting must go to the PTY without model or UI
reinterpretation.

### 2. Renderer and session authority are separate

The Rust kernel should own the PTY, process group, ordered transcript, terminal
state, and session lifetime. A native renderer, WebView, or remote client is a
replaceable projection that reconnects from a snapshot plus ordered deltas.

### 3. Semantic overlays degrade gracefully

Shell integration can identify prompt, command, cwd, and exit boundaries. If
it fails, raw Terminal behavior must still work. OSC markers and terminal
output are untrusted hints: they may improve navigation, never authorize an
action or assert agent identity.

### 4. Completion is a terminal-specific contract

Shell completion, command specifications, filesystem suggestions, project
indexing, and AI proposals are independent providers. Each declares its
capabilities and returns an edit proposal, source, confidence, and risk hints.

### 5. AI CLIs are alternate workloads, not the canonical integration

Codex and Claude should always work as ordinary terminal programs. When a
structured interface is available—Codex app-server, ACP, provider event streams,
or hooks—the Control Plane attaches it as a parallel channel instead of scraping
the TUI.

## What not to copy

- Do not let a WebView or editor own shell line editing.
- Do not make command cards the terminal emulator's primary storage model.
- Do not treat bounded scrollback as durable history.
- Do not trust OSC/private escape sequences as proof of actor or authority.
- Do not let plugins or models directly call `sendText(...execute)`.
- Do not migrate AI capabilities by swapping the whole terminal engine.
- Do not infer agent completion from quiet output, a prompt glyph, or a window
  title.

## Recommended Hyper Term architecture

```text
Human keyboard ───────────────────────────────┐
                                             ▼
                                    Terminal Data Plane
                                    PTY · signals · resize
                                    VT state · raw transcript
                                             │
                                             ▼
                                  Screen snapshot + deltas
                                             │
                                             ▼
                              Native grid / WebView projection

Mission Composer ─┐
Voice Draft ──────┼──► AI Control Plane ───► Operation Intent
Agent adapters ───┘    Task reducer              │
                       Permission broker         ▼
                       Input lease          Execution drivers
                       Attention reducer    PTY · Computer · Browser · MCP
                       Operation ledger          │
                       Evidence store ◄──────────┘

Shell integration / OSC ──► untrusted Semantic Hints ──► visual grouping only
Structured agent protocol ─► authenticated Agent Events ─► task state + attention
```

### Rust ownership boundary

The Rust process should own:

- PTY/session/process-group lifetime;
- terminal emulator state and raw transcript chunks;
- snapshot/delta sequencing and reconnect;
- Task and Operation reducers;
- permissions and immutable approvals;
- input leases for Human versus Agent;
- durable ledger, artifacts, redaction, and retention;
- structured Agent, Computer, Browser, SSH, and MCP drivers.

The UI should own:

- terminal grid rendering and selection;
- task rail, composer, attention inbox, and review surfaces;
- expanding an Operation into raw output or evidence;
- accessible labels, keyboard navigation, and visual policy;
- transient layout state.

WebView is acceptable at this boundary because it can crash and reconnect
without killing the PTY or losing the ledger. It is not acceptable as the
process authority, shell editor, or durable transcript store.

### Minimum extension contracts

```rust,ignore
trait AgentAdapter {
    fn capabilities(&self) -> AgentCapabilities;
    async fn attach(&mut self, target: ExecutionTarget) -> Result<AgentSession>;
    async fn control(&mut self, command: AgentControl) -> Result<()>;
    fn events(&mut self) -> EventStream<AgentEvent>;
}

trait ExecutionDriver {
    fn capabilities(&self) -> CapabilitySet;
    async fn start(&mut self, intent: ApprovedOperation) -> Result<OperationHandle>;
    async fn signal(&mut self, operation: OperationId, signal: ControlSignal) -> Result<()>;
    fn events(&mut self) -> EventStream<ExecutionEvent>;
}

trait CompletionProvider {
    fn capabilities(&self) -> CompletionCapabilities;
    async fn propose(&self, context: CompletionContext) -> Result<Vec<EditProposal>>;
}
```

Every provider receives the minimum capabilities it needs. Filesystem listing,
process execution, network access, secret access, and command execution are
separate grants. A completion plugin is not implicitly an execution plugin.

## Recommended interaction model

Use explicit composer modes:

- **Shell** — raw human input to the PTY;
- **Ask** — discussion with no execution authority;
- **Delegate** — create or steer a durable agent task;
- **Drive** — request Computer Use against a named surface.

When an agent needs the terminal, it acquires an `InputLease`. The user can
`Steer`, `Pause`, `Interrupt`, `Take over`, or `Abort`; these actions are not
collapsed into `Ctrl+C`. Voice creates or edits a Task Contract and may invoke
local control actions, but it never bypasses the permission broker or directly
injects a shell command.

An AI operation appears as a compact block over the terminal projection:

```text
Agent Codex · Fix authentication · needs approval
Proposes: cargo test auth
Target: local · worktree auth-fix · risk low
[Approve once] [Edit] [Open task] [Take over]
```

After execution, the same block becomes an evidence projection. Raw bytes stay
available, but review defaults to intent, diff, tests, effects, and unresolved
risks.

## First product slice after this audit

Do not start by cloning IDEA's renderer. Validate the semantic boundary:

1. keep the existing Rust PTY/session core transparent;
2. add ordered screen snapshots and deltas so any UI can reconnect;
3. ingest shell command boundaries as untrusted hints;
4. add one structured Codex driver beside the opaque TUI fallback;
5. create an Operation before any AI command reaches the PTY;
6. show Human versus Agent actor, intent, approval, exit, and evidence;
7. implement `Take over` with an explicit input lease;
8. generate `NeedsInput`, `NeedsApproval`, `Failed`, and `ReviewReady`
   attention items from structured state.

This slice tests the key differentiation that IDEA does not yet provide:

> Can one terminal remain fully compatible while also explaining and
> controlling AI work as durable, reviewable operations?

## Evidence limits

- The GUI audit covered the installed macOS 2026.1 build, Zsh, local execution,
  and one Codex TUI launch. It did not test SSH, Docker, WSL, remote development,
  tmux, Vim, long output, resize stress, process orphaning, or multiple agents.
- Visual and accessibility-tree inspection cannot establish WCAG compliance or
  screen-reader usability.
- Community and YouTrack issue clusters help identify regression categories,
  but they are not quantitative incidence data.
- JetBrains' Reworked Terminal API remains experimental, so class and extension
  details should be treated as a current reference rather than a stable contract.
