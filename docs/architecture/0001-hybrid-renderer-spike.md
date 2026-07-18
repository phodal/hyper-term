# ADR 0001: Spike with xterm.js, target a native terminal hot path

- Status: accepted for spike; native renderer decision pending bake-off
- Date: 2026-07-18

## Context

The empty repository needed a fast, end-to-end proof of a real PTY, WebView,
typed IPC, resize, lifecycle, and explicit AI context boundary. Building a
correct terminal parser, grid, font shaper, renderer, IME path, and accessibility
tree at the same time would hide the product questions behind renderer work.

At the same time, making xterm.js the permanent core would bind terminal
latency, memory, and renderer semantics to a system WebView. That conflicts with
the strongest current native designs in Ghostty, Warp, Rio, and cmux.

## Decision

The first runnable adapter uses:

- `portable-pty 0.9` in renderer-independent Rust core;
- Tauri 2 as a narrow desktop adapter;
- a Tauri Channel for ordered, high-throughput PTY output;
- React + xterm.js 6 for the disposable terminal renderer experiment;
- a bounded Rust transcript for explicit context capture;
- no provider and no AI execution permission.

Tauri's own documentation says ordinary events are not intended for low-latency,
high-throughput data and recommends [Channels for ordered streaming](https://v2.tauri.app/develop/calling-frontend/#channels).
xterm.js is explicitly a frontend component that expects a separate PTY, and
provides WebGL, Unicode, IME, accessibility, and addon coverage useful for the
spike: [xterm.js](https://github.com/xtermjs/xterm.js/).

The target product keeps the terminal hot path native:

```text
winit input → session actor → terminal model → wgpu cell renderer
                                      └──────→ semantic event log

Wry/Tauri WebView (separate capability)
  └── agent conversation, diff, Markdown, diagrams, browser, settings
```

## Why the Rust core is separate now

`hyper-term-core` has no Tauri dependency. The WebView cannot spawn a program;
it can only call a small adapter surface: start, write, resize, capture bounded
context, and stop. This lets a future winit/wgpu client or daemon reuse the same
session contracts.

## Security consequences

- Tauri capabilities are restricted to the local main window and core defaults.
- CSP has no remote script source.
- The WebView receives bytes and DTOs, never terminal-generated HTML.
- Context capture is explicit, local, capped at 64 KiB, and marked untrusted.
- A future model returns `ProposedCommand`; only a separate policy/permission
  broker may execute it.
- OSC semantic markers will improve presentation but cannot establish actor
  provenance because any child process can print them.

Tauri describes the Rust/frontend boundary as a trust boundary and recommends
window-specific least privilege: [security model](https://v2.tauri.app/security/)
and [capabilities](https://v2.tauri.app/security/capabilities/).

## Exit gate for the renderer spike

Before committing to a production renderer, measure the same workloads against
xterm.js and a native adapter:

- p50/p95/p99 key-to-present latency;
- 10 MiB and 100 MiB burst behavior with zero lost PTY bytes;
- idle CPU and energy;
- scrollback memory at 100k and 1M lines;
- CJK, emoji, ligatures, IME, accessibility, and resize correctness;
- WebView crash/reload behavior and channel backpressure.

The native bake-off is `wezterm-term` versus `alacritty_terminal`, both behind a
local `TerminalModel` trait. The first renderer experiment should use
`winit + wgpu + cosmic-text/glyphon`; production will likely need a cell-aware
glyph atlas rather than paragraph layout.
