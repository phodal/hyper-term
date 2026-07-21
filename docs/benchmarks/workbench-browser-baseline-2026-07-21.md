# Workbench browser release baseline — 2026-07-21

This gate proves the built Agentic UI Workbench is more than a static mock. It
drives the browser-facing part of the Native SDK application while keeping PTY,
filesystem, process, artifact acceptance, and permission authority outside the
WebView.

## Environment

- macOS 26.5.2 (25F84), Apple M4 Max (`arm64`)
- Deno 2.9.3 built-in bundler
- agent-browser 0.25.4 using Chromium
- CodeMirror 6 and esbuild-wasm 0.28.1
- built `dist/workbench` assets served from an ephemeral loopback port

## Reproduce

```bash
deno task build:workbench
deno task verify:workbench-browser
```

The verifier opens the exact built HTML, Worker, preview shell, and WASM
compiler. It requires the initial artifact to compile, focuses the real
CodeMirror editor, replaces the document with CJK JSX, and waits for both the
accepted build status and isolated preview `ready` handshake. The accessibility
snapshot must expose the generated Chinese heading inside the sandboxed iframe.

It then opens the real CodeMirror merge view and confirms the original and
modified documents remain editable projections. Finally it switches to a
480-by-900 viewport, proves there is no document-level horizontal overflow,
scrolls the nested workspace surface, and requires the complete Studio to enter
the viewport. The test retains wide Diff and narrow Studio screenshots.

## Initial result

| Gate | Result |
| --- | --- |
| Built page and esbuild-wasm Worker | pass |
| CJK CodeMirror edit | pass |
| Live preview reload and iframe ready handshake | pass |
| Editable original/modified Diff | pass |
| 480 px Studio reachability | pass |
| Document-level horizontal overflow | none |
| Browser page errors | none |

This is a same-build correctness alarm, not a general browser compatibility or
performance benchmark. The macOS release pairs it with Native SDK automation
and Rust-owned artifact and permission tests. The separate Terminal browser
gate exercises the real zsh-to-xterm path during local release qualification.
