# AI-era terminal landscape — 2026-07-18

The user mentioned “Warm”; this spike treats that as **Warp**. Version and
feature claims below were checked against project documentation or release
pages on 2026-07-18.

## Executive read

The market has split into three layers:

1. a fast terminal engine and durable session substrate;
2. a workspace for terminal, browser, editor, previews, and review;
3. an agent runtime with context, permissions, background work, and attention.

The opportunity is not another terminal plus a chat box. It is a provider-
neutral workbench whose core state is a typed, append-only event stream. The
terminal grid, command blocks, agent threads, and audit history are projections
of that stream.

## Comparison

| Product | Current shape | What to borrow | Constraint or warning |
|---|---|---|---|
| [Warp](https://docs.warp.dev/) | Rust agentic development environment; client [became open source in April 2026](https://github.com/warpdotdev/warp/discussions/9240) | [Atomic command/output blocks](https://docs.warp.dev/terminal/blocks), attachable block context, [full PTY control for agents](https://docs.warp.dev/agent-platform/capabilities/full-terminal-use), attention and review surfaces | Large custom GPU/UI investment; repository contains AGPL and MIT scopes. ACP was still a [roadmap item](https://github.com/warpdotdev/warp/issues/9233), so do not couple to Warp internals. |
| [cmux](https://github.com/manaflow-ai/cmux) | Native macOS AppKit app using `libghostty`, with terminals, workspaces, notifications, and browser panes | Clean native terminal/WebView split, agent-independent attention model, CLI/socket control, browser verification surface | macOS-only and GPL/commercial dual licensing; `libghostty` C API still evolves. |
| [Wave Terminal](https://github.com/wavetermdev/waveterm) | Go + TypeScript/React/Electron workbench; v0.14.5 on 2026-04-16 | Terminal, editor, web, remote files, previews, widgets, BYOK, and local model support in one workspace | Electron footprint and a wider web security surface. Its 2026 macOS artifact was around 184–199 MB, useful as a packaging comparison. |
| [Ghostty](https://github.com/ghostty-org/ghostty) | Zig core, Metal/OpenGL native apps, embeddable `libghostty` | “Terminal library + native host” boundary, per-terminal read/write/render threads, modern protocol coverage | `libghostty` is usable but not yet independently versioned; avoid binding product APIs directly to its unstable C surface. |
| [WezTerm](https://github.com/wezterm/wezterm) | Rust GPU terminal and local/remote multiplexer | Mature terminal model, reconnectable multiplexer domains, Lua events/config, broad escape-sequence support | Stable release still points to 2024 even though main remained active in 2026; internal crates need pinning and adapters. |
| [Rio](https://github.com/raphamorim/rio) | Rust/wgpu terminal; v0.4.5 on 2026-05-20 | Current Rust renderer, font, IME, and cross-platform reference | Active architectural churn; browser/WASM goal was not yet a complete terminal port. |
| [Kitty](https://sw.kovidgoyal.net/kitty/overview/) | Native OpenGL terminal with programmable kittens and remote control | [Graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/), keyboard protocol, remote-control framing, latency/energy benchmarking | GPL and many terminal-specific protocols; remote-control and escape surfaces need strict policy. |
| [Tabby](https://github.com/Eugeny/tabby) | Electron/xterm.js terminal with SSH, serial, SFTP, vault, and plugins | Breadth of remote transports and a practical extension ecosystem | Electron hardening matters: its history includes a [TCC bypass advisory](https://github.com/Eugeny/tabby/security/advisories/GHSA-prcj-7rvc-26h4). |
| [Zed](https://zed.dev/docs/terminal) | Rust/GPUI editor with integrated terminal and agent | Terminal tasks, code context, agent thread, MCP, and review as one workflow | Editor-first product; GPUI is still a pre-1.0 dependency and not a safe cross-platform contract yet. |
| [VS Code terminal](https://code.visualstudio.com/docs/terminal/basics) | xterm.js + PTY host inside Electron | [Shell integration](https://code.visualstudio.com/docs/terminal/shell-integration/) for command/cwd/exit semantics; mature accessibility, IME, and extension patterns | Heavy process/plugin model; terminal output, extensions, and WebViews all cross trust boundaries. |
| [Microsoft Intelligent Terminal](https://devblogs.microsoft.com/commandline/announcing-intelligent-terminal-version-0-1/) | Experimental Windows Terminal fork; 0.1 in June 2026 and [0.1.1](https://devblogs.microsoft.com/commandline/intelligent-terminal-0-1-1-is-here-bash-support-new-slash-commands-and-customization/) later that month | ACP-compatible agent adapters, automatic error detection, background agent tabs, persistent attention/status bar | Windows-only and explicitly experimental. |

## Product conclusions

### Build these as first-class primitives

- **Command blocks:** command, cwd, actor, byte range, exit status, duration,
  trust, artifacts, and linked agent run.
- **Attention:** running, waiting for input, waiting for approval, failed,
  completed, and unread are product state, not notification strings.
- **Durable sessions:** GUI restarts must not own PTY lifetime in the target
  architecture.
- **Explicit context:** a user selects blocks/files/ranges; the system reports
  bytes/tokens and redactions before any provider call.
- **Provider-neutral agents:** raw CLI works through PTY; structured coding
  agents plug in through [ACP](https://github.com/agentclientprotocol/agent-client-protocol);
  external tools/data use [MCP](https://modelcontextprotocol.io/specification/2025-06-18).
- **Rich surfaces:** terminal, diff, editor, Markdown, image, diagram, browser,
  and generated UI can share a workspace without sharing authority.

### Avoid these traps

- Treating the character grid as the only source of truth.
- Letting a model infer command boundaries by scraping pixels or scrollback.
- Giving an AI WebView a generic `exec`, filesystem, or Node bridge.
- Treating OSC, hyperlink text, model output, or a remote page as trusted UI.
- Using a command-string allowlist as a security sandbox.
- Starting with a custom all-purpose Rust UI framework before the terminal
  model, session protocol, and product semantics have been proven.
- Copying GPL/AGPL implementation code without an explicit licensing decision.

## Recommended synthesis

Use Ghostty/WezTerm's terminal-library boundary, Warp's typed block/event model,
cmux's native terminal plus browser/attention surfaces, OpenCode-style headless
sessions, and Intelligent Terminal's ACP adapter. Hyper Term should be able to
host Codex, Claude Code, OpenCode, Gemini CLI, or a future local agent without
changing its terminal substrate.
