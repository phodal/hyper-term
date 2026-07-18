# AI CLI and Computer Use probe — 2026-07-18

## Question

Should Hyper Term render Codex and Claude as ordinary terminal applications, or
should it own the AI interaction model and use the terminal as one executor?

The probe used empty directories under `/private/tmp`, read-only/no-tool prompts,
and the installed CLIs. It did not expose the Hyper Term workspace to either
agent.

## Local versions and a version-drift event

At the start of the probe:

```text
codex-cli 0.144.5
Claude Code 2.1.207
```

Launching Claude caused its existing background updater to replace the local
symlink with version `2.1.214`. No explicit `claude update` command was run.
This matches Claude's documented behavior of checking and installing updates in
the background, with the new version taking effect on the next launch:
[Claude Code setup and updates](https://docs.anthropic.com/en/docs/claude-code/getting-started).

After the update, the new Claude binary reported that it was not logged in. The
probe did not start a login flow or change credentials. This prevented a second
authenticated structured-output turn, but the error path still exposed the wire
protocol.

Architectural consequence: every external driver must record the executable
version and negotiate capabilities at attach time. Never hard-code a TUI layout,
flag set, or JSON schema without a compatibility probe and fixtures.

## Codex interactive TUI

The isolated command was equivalent to:

```sh
codex -C /private/tmp/<empty-dir> \
  --sandbox read-only \
  --ask-for-approval never \
  'Do not call tools. Reply exactly: CODEX_TUI_OK'
```

It completed with `CODEX_TUI_OK`. The raw PTY stream exercised:

```text
CSI ?2004h/l      bracketed paste
CSI >4;0m         modifyOtherKeys
CSI >7u / CSI ?u  Kitty keyboard enhancement and query
CSI ?1004h/l      focus reporting
CSI 6n            cursor position query
OSC 10/11         foreground/background color query
CSI c             device attributes
CSI ?2026h/l      synchronized output
scroll regions, reverse index, cursor shape, title updates
```

The main Codex view used an inline viewport. Its source confirms that alternate
screen is entered for selected transcript, diff, picker, and approval overlays,
not as the permanent main surface. Codex is a `crossterm + ratatui` application,
not a terminal emulator; `vt100` is a development dependency:
[Codex TUI Cargo.toml](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/tui/Cargo.toml#L72),
[terminal setup](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/tui/src/tui.rs#L163), and
[terminal probe](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/tui/src/terminal_probe.rs#L1).

The terminal compatibility layer therefore has to answer terminal queries and
correctly implement synchronized output, enhanced keyboard modes, focus,
alternate screen, resize, and wide text. These details belong below the AI UI.

## Codex structured channel

The non-interactive JSONL probe used `codex exec --json` with an ephemeral,
read-only session. Its useful event sequence was:

```text
thread.started
turn.started
item.completed(type=agent_message, text=CODEX_JSON_OK)
turn.completed(usage=...)
```

The richer `codex app-server` is explicitly the interface used for rich clients.
It exposes thread, turn, item, plan, diff, approval, interrupt, resume, fork, and
token events over JSONL or local/remote transports:
[Codex app-server protocol](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md).

Codex source also shows why these events matter:

- queued/steer input is a composer state, not terminal output:
  [input flow](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/tui/src/chatwidget/input_flow.rs#L71);
- approvals carry stable thread, turn, item, approval, and decision fields:
  [approval protocol](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/app-server-protocol/src/protocol/v2/item.rs#L1253);
- focus-gated attention is reduced from explicit event types:
  [notifications](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/tui/src/chatwidget/notifications.rs#L5);
- unresolved approvals and questions are replayed when a thread is restored:
  [pending replay](https://github.com/openai/codex/blob/cccde930ce575eec18f8ecfa4838f8e155ab771e/codex-rs/tui/src/app/pending_interactive_replay.rs#L24).

Scraping ANSI would discard all of these identifiers and state transitions.

## Claude interactive TUI

The authenticated interactive probe ran in safe mode with no tools and plan-only
permissions. It completed with `CLAUDE_TUI_OK`.

The TUI used bracketed paste, focus reporting, color-scheme notifications,
terminal capability queries, cursor save/restore, absolute positioning, and
mouse modes. Its full-screen renderer entered `CSI ?1049h`, captured
1000/1002/1003/1006 mouse modes, kept a fixed composer, redrew the visible
viewport, updated the terminal title during work, and restored all modes on exit.

The resize probe changed the PTY dimensions and sent `SIGWINCH`; Claude cleared
and reflowed the viewport. Raw fixtures were retained only in the isolated local
probe directory and are not part of this repository.

Claude documents the compatibility differences and scrollback trade-offs of
this renderer: [Claude full-screen rendering](https://code.claude.com/docs/en/fullscreen).

`--ax-screen-reader` produces flatter output but still uses terminal modes. A
native GPU grid therefore does not automatically provide a useful accessibility
tree; accessibility is a separate projection of the screen model.

## Claude structured channel

Claude exposes bidirectional `stream-json`, partial messages, hook events,
session resume, background agents, JSON Schema output, and structured permission
handling: [Claude CLI reference](https://code.claude.com/docs/en/cli-usage).

The post-update, unauthenticated probe still produced:

```text
system.init(capabilities, tools, permissionMode, model)
system.status(requesting)
assistant(error=authentication_failed)
result(is_error=true, terminal_reason=api_error)
```

The process returned exit code `0`, and the result subtype was `success`, even
though `is_error` was true. A driver must reduce the typed fields rather than
equating process exit with task success.

Claude's background Agent view and Remote Control further separate a durable
runtime from any one terminal client:
[Agent view](https://code.claude.com/docs/en/agent-view) and
[Remote Control](https://code.claude.com/docs/en/remote-control).

## Computer Use observation of the current spike

The running Hyper Term Tauri spike was inspected through macOS Computer Use.
Its accessibility tree exposed the native window, product controls, terminal
input field, context buttons, and a captured raw PTY tail. The xterm WebGL grid
itself did not expose a semantic terminal screen through accessibility; the
captured tail contained raw escape sequences.

This is a useful failure, not merely a WebView problem:

- a screenshot gives pixels but no stable task or approval identity;
- an accessibility tree gives semantic controls where the app exposes them;
- a terminal screen model gives cells, selection, cursor, and scrollback;
- an agent protocol gives plans, tools, approvals, diffs, and lifecycle state.

Computer Use needs a broker that can combine these observations without
pretending they are interchangeable. It should prefer semantic element actions,
refresh state after actions, and fall back to coordinates only when required.

Current Computer Use systems use an iterative perception → action → observation
loop. Anthropic's reference explicitly recommends a sandboxed environment,
human confirmation for consequential actions, and verification after actions:
[Claude computer use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool).
OpenAI describes the same screenshot/reason/action loop and layered confirmation
model: [Computer-Using Agent](https://openai.com/index/computer-using-agent/).

## Product conclusion

Two planes are necessary:

1. **Compatibility plane** — PTY + complete terminal model for arbitrary CLIs,
   TUIs, SSH, tmux, editors, and REPLs.
2. **Control plane** — structured Agent, tool, Computer Use, permission,
   evidence, attention, and resume events owned by `hyperd`.

Computer Use is a third executor beside Agent and Terminal, not a screen-scraping
replacement for them. It should handle long-tail GUI work after dedicated APIs,
agent protocols, CLI commands, DOM, and accessibility actions have been
considered.

The recommended interaction and extension model is summarized in the
[project roadmap](../../README.md#roadmap) and the
[versioned Block Render decision](../architecture/0011-versioned-block-render-document.md).
