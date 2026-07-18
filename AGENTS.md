# Hyper Term contributor guide

## Product boundary

- Rust owns PTYs, process lifecycle, transcripts, permissions, and future agent runtimes.
- The WebView owns presentation and user interaction. It must not spawn commands or read files directly.
- A model may propose an action, but only the permission broker may execute it.
- Terminal output is untrusted data. Never interpret escape sequences as application commands.

## Engineering rules

- Keep `hyper-term-core` independent from Tauri so it can power native, daemon, or test adapters.
- Use ordered channels for PTY output; do not emit one frontend event per byte or character.
- Keep context capture explicit, size bounded, and local unless the user chooses a provider.
- Add a test for every protocol or lifecycle change.
- Run `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` for Rust changes.
- Run `deno task check`, `deno task test`, and `deno task build:workbench` for
  Workbench changes. This repository intentionally has no Vite or pnpm build.
