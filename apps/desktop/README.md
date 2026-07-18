# Hyper Term desktop

This is the macOS-first Native SDK product shell. The default view is a normal
Terminal; New Session explicitly offers Terminal or Agent mode.

The current slice implements the trusted native chrome, design tokens, mode
selection, responsive layout contracts, and Agent Block composition shape. Its
terminal and agent panes are honest disconnected placeholders. Zig does not
spawn a shell: the next integration attaches them to Rust `hyperd` over the
ordered control and terminal planes defined in ADR 0013.

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
