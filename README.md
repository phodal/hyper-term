<div align="center">
  <img src="apps/desktop/assets/icon.png" width="112" alt="Hyper Term icon" />

<h1>Hyper Term</h1>

<p><strong>A local-first terminal for humans and coding agents.</strong></p>
  <p>A normal terminal by default. An Agent workspace only when you choose it.</p>

<p>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="Apache-2.0 license" /></a>
    <img src="https://img.shields.io/badge/platform-macOS-black.svg" alt="macOS" />
    <img src="https://img.shields.io/badge/status-alpha-orange.svg" alt="Alpha status" />
  </p>
</div>

Hyper Term is an open-source, macOS-first terminal. Open it and you get a real
login shell. When you need a coding agent, create a separate Agent tab for
structured messages, approvals, diffs, and generated interfaces.

<p align="center">
  <img src="docs/assets/hyper-term-ui.svg" width="100%" alt="Hyper Term native terminal interface" />
</p>

> [!IMPORTANT]
> Hyper Term is in active development. You can build and run it from source, but
> it is not ready for production use or general distribution.

## Why Hyper Term?

- **Terminal first.** New tabs open the user's login shell with normal job
  control, signals, resize, UTF-8, CJK/IME input, search, and scrollback.
- **Agent mode is explicit.** A normal Terminal tab never starts a model or
  changes shell behavior.
- **You stay in control.** Agents propose operations; Rust checks permissions
  and waits for approval before execution or workspace writes.
- **Local by default.** PTYs, process lifecycle, transcripts, and accepted
  artifacts stay under the local Rust core. WebViews only render trusted data.

Agent tabs can connect to locally installed Codex, Claude, and GitHub Copilot
CLIs. They present plans and tool calls as structured, searchable blocks, and
can open generated React/TypeScript artifacts in an isolated editor and preview.
Press `Command-F` in an Agent tab to filter its retained messages, tools, files,
and approvals; ordinary Terminal tabs keep terminal-native find. Terminal
rendering stays on the fast WebGL path by default. Screen-reader users can press
`Shift-Tab` from Terminal input to reveal **Enable screen reader mode**, then
press `Enter`; the preference is local and adds xterm's navigable row list and
live output region without slowing every terminal session.

## How it works

```mermaid
flowchart LR
    OPEN["Open Hyper Term"] -->|"Default"| TERMINAL["Terminal<br/>login shell"]
    OPEN -->|"Choose New Agent"| AGENT["Agent workspace"]
    AGENT --> PROVIDER["Codex · Claude · Copilot"]
    PROVIDER --> PROPOSAL["Structured proposal"]
    PROPOSAL --> APPROVAL{"User approves?"}
    APPROVAL -->|Yes| RUST["Rust executes<br/>the exact operation"]
    APPROVAL -->|No| AGENT
    TERMINAL --> CORE["Rust PTY core"]
    RUST --> RESULT["Review result or diff"]
```

The key boundary is simple: the UI and agent may propose an action, but only the
Rust permission broker can execute it. Terminal output is always treated as
untrusted data.

## Get started

### Requirements

- macOS
- Rust `1.95` (pinned by `rust-toolchain.toml`)
- Deno `2.9.3`
- Zig `0.16.0`
- Native SDK CLI `0.5.3`

Clone the repository and check the Deno runtime:

```bash
git clone https://github.com/phodal/hyper-term.git
cd hyper-term
deno task verify:runtime
```

Build the Terminal, Workbench, and native application:

```bash
deno task build:terminal
deno task build:workbench
(cd apps/desktop && native build --release=fast)
```

Start Hyper Term:

```bash
cargo run -p hyper-term-daemon --bin hyper-term-desktop -- \
  --ui "$PWD/apps/desktop/zig-out/bin/hyper-term" \
  --terminal-assets "$PWD/dist/terminal" \
  --workbench-assets "$PWD/dist/workbench"
```

The app opens as a normal terminal; no agent provider is required. To use an
Agent tab, install and sign in to a supported provider CLI first. Run
`cargo run -p hyper-term-daemon --bin hyper-term-desktop -- --help` to see
provider-path options.

### Build a local macOS app

```bash
./scripts/package_macos_app.sh
open "dist/macos/Hyper Term.app"
```

This creates an ad-hoc signed development build. Public signed and notarized
releases are not available yet.

## Development

Run the Rust checks:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run the Deno checks:

```bash
deno task verify:runtime
deno task check
deno task test
deno task build:workbench
deno task verify:workbench-browser
```

Hyper Term uses Deno's frozen lockfile and built-in bundler. There is no Vite or
pnpm build. The optional browser gate requires `agent-browser`; it edits the
built Workbench, waits for the esbuild-wasm live preview, opens the editable
Diff, and verifies that Studio remains reachable at 480 px.

## Project map

```text
apps/desktop/               Native macOS application
apps/terminal/              Terminal renderer
apps/workbench/             Agent blocks, editor, and artifact preview
crates/hyper-term-core/     PTYs, state, and renderer-independent authority
crates/hyper-term-daemon/   Daemon, desktop supervisor, and local gateways
crates/hyper-term-drivers/  Agent, MCP, Deno LSP, and GenUI adapters
crates/hyper-term-protocol/ Shared contracts
crates/hyper-term-sandbox/  OS sandbox backends
docs/                       Architecture, research, and release notes
```

Start with these documents when you want more detail:

- [Product and interaction design](DESIGN.md)
- [Runtime authority boundaries](docs/architecture/0002-runtime-authority-boundaries.md)
- [Native SDK product shell](docs/architecture/0013-native-sdk-default-product-shell.md)
- [Coding-agent sandbox](docs/architecture/0014-rust-owned-coding-agent-sandbox.md)
- [macOS release process](docs/release/macos-app.md)

## Current status

The PTY kernel, native Terminal tabs, structured Agent tabs, provider adapters,
and isolated artifact preview all have runnable baselines. Workspace apply,
replay, sandboxing, and the Workbench are still experimental. The Rust desktop
supervisor keeps PTY and Agent gateways alive while it performs a bounded
restart of a crashed Native renderer. Terminal and Agent tab layout, the active
tab, and Agent session bindings are restored across that renderer replacement.
The same layout and active tab also survive a full application restart through
private Rust-owned state. Agent tabs reattach their existing Rust
`BlockDocument` history through a bounded private Task ID binding, while the
provider process and every Terminal PTY still start fresh instead of pretending
their old processes survived. The current focus is reliability, accessibility,
containment, and signed macOS distribution.

## Contributing

Issues and focused pull requests are welcome. Read [AGENTS.md](AGENTS.md) before
changing protocols or process lifecycle behavior, and add a regression test for
every such change. Keep `hyper-term-core` independent from the renderer and keep
command and filesystem authority out of WebViews.

## License

Hyper Term is licensed under the [Apache License 2.0](LICENSE). Third-party
components and notices are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
