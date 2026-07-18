# Hyper Term desktop

This is the macOS-first Native SDK product shell. The default view is a normal
Terminal; New Session explicitly offers Terminal or Agent mode.

The native chrome, design tokens, mode selection, responsive layout, and Agent
Block composition remain native. The terminal cell renderer is currently a
child system WebView anchored into that layout. It connects directly to the
authenticated loopback terminal plane; terminal bytes never cross the Native
SDK JSON bridge, and Zig never spawns a shell.

For a development launch, build `dist/terminal`, start `hyperd` with its default
`127.0.0.1:47437` gateway, then provide the same token to the desktop process:

```sh
HYPER_TERM_TERMINAL_URL='http://127.0.0.1:47437/?token=<32+ byte token>' native dev
```

Without that exact local URL the app keeps an honest disconnected Terminal
placeholder. A future native cell-grid renderer can replace the WebView without
changing the Rust PTY or reconnect protocol.

## Commands

The project is zero-config so it never commits a developer-machine SDK path. Use
the pinned Native SDK CLI and Zig 0.16 toolchain:

```sh
npx -y @native-sdk/cli@0.5.3 check --strict
npx -y @native-sdk/cli@0.5.3 test
npx -y @native-sdk/cli@0.5.3 dev
npx -y @native-sdk/cli@0.5.3 build --release=fast
```

Debug builds hot-reload `src/app.native` while retaining the Zig model. Release
builds compile the same markup ahead of time. Generated `.native/`, Zig cache,
and build output stay untracked.
