# Hyper Term desktop

This is the macOS-first Native SDK product shell. The default view is a normal
Terminal. `New` creates another ordinary Terminal tab, while the adjacent
`Agent` action explicitly creates a brokered Agent tab. Every tab exposes its
own close control and context-menu action; Command-W closes the active tab
through the same Rust-owned session lifecycle. The terminal surface adds
Command-F scrollback search and persistent Command-Plus / Command-Minus zoom
without claiming Ctrl-C or other shell input.

Agent tabs are single-pane by default, matching the disclosure behavior of a
modern coding-agent client rather than reserving a permanent sidebar. When an
ACP-backed Agent has a current editable artifact, the conversation exposes an
explicit Edit action; the right editor pane is mounted only after the user
enters that editing state and can be closed without closing the Agent tab. That
pane is the packaged Deno-built Workbench; CodeMirror, Diff, Time Travel, and
its isolated local preview have no workspace-write authority. CodeMirror
diagnostics and completion come from a Rust-supervised Deno LSP process against
the current artifact's private snapshot. Draft updates travel as bounded LSP
document changes, not filesystem writes.

The conversation follows a response-first density model: assistant prose is
unboxed, reasoning and tool activity are grouped into compact disclosures, and
the active Goal is one summary row until expanded. The reading surface and
composer stretch with the window instead of preserving a narrow fixed rail. The
composer keeps commands and Skills at the leading edge while provider-owned
model and reasoning controls sit beside Send. Controls are rendered only when a
real provider capability exists; attachment, voice, and permission affordances
must not be simulated by inert chrome.

Each structured Agent session also receives Hyper Term as a digest-pinned stdio
MCP server. Its Diff and GenUI tools operate only on bounded request data. The
Deno LSP tool sees a private per-session text snapshot created by Rust, not the
live workspace; dependency/build directories, binary files, and symlinks are
excluded. Calls remain visible approval operations in the Agent tab, and the
snapshot and runtime roots are removed when that Agent session closes.

The native chrome, design tokens, mode selection, responsive layout, and Agent
Block composition remain native. The terminal cell renderer is currently a child
system WebView anchored into that layout. It connects directly to the
authenticated loopback terminal plane; terminal bytes never cross the Native SDK
JSON bridge, and Zig never spawns a shell.

For an integrated development launch, build the terminal and native renderer,
then let the Rust desktop supervisor own daemon and renderer lifetime:

```sh
deno task build:terminal
deno task build:workbench
(cd apps/desktop && native build --release=fast)
cargo run -p hyper-term-daemon --bin hyper-term-desktop -- \
  --ui "$PWD/apps/desktop/zig-out/bin/hyper-term" \
  --terminal-assets "$PWD/dist/terminal" \
  --workbench-assets "$PWD/dist/workbench"
```

The supervisor creates a per-launch gateway token, starts new login shells in
the user home directory, and passes only the authenticated loopback URL to the
Native renderer. Without that exact local URL the app keeps an honest
disconnected Terminal placeholder. A future native cell-grid renderer can
replace the WebView without changing the Rust PTY or reconnect protocol.

To inspect an exported Bug Capsule without starting an ACP provider, append an
absolute path to the integrated launch command:

```sh
--bug-capsule /absolute/path/to/report.bug-capsule.json
```

Rust opens and verifies the file before launching the renderer. Native then uses
a dedicated Capsule tab whose Workbench pane is replay-only; the pane gets the
launch token and bounded capsule projection, never the local path or direct
filesystem authority.

Build an ad-hoc signed macOS application from the repository root:

```sh
./scripts/package_macos_app.sh
open "dist/macos/Hyper Term.app"
```

The package keeps `hyper-term` as the Rust-owned bundle entry point and installs
the Native SDK executable as `hyper-term-ui`. Terminal assets are copied into
`Contents/Resources/terminal`; no development server or global Node runtime is
required at launch. The trusted editor assets are packaged separately under
`Contents/Resources/workbench` and served only by the Rust Agent gateway.

## Commands

The project is zero-config so it never commits a developer-machine SDK path. Use
the pinned Native SDK CLI and Zig 0.16 toolchain:

```sh
npx -y @native-sdk/cli@0.5.3 check --strict
npx -y @native-sdk/cli@0.5.3 test
npx -y @native-sdk/cli@0.5.3 dev
npx -y @native-sdk/cli@0.5.3 build --release=fast
```

Debug builds register `src/app.native` as a watched compiled fragment. A saved
edit is parsed at runtime, but the refreshed fragment still passes through the
Zig `rootView` that composes the Agent Block timeline. Release builds compile
the same markup ahead of time and carry no source path or watcher. Generated
`.native/`, Zig cache, and build output stay untracked.
