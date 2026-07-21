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

## Scale result — 2026-07-22

The same built Worker now accepts the bounded 1,000-file contract and records
initial plus five consecutive rebuilds for complete linked graphs:

Two consecutive successful runs produced these ranges:

| Modules | Initial | Rebuild p50 | Rebuild p95/max |
| ---: | ---: | ---: | ---: |
| 100 | 1,081.8–1,105.1 ms | 1,046.8–1,055.6 ms | 1,090.0–1,973.3 ms |
| 500 | 5,130.2–5,169.3 ms | 5,018.7–5,063.1 ms | 5,087.6–5,194.1 ms |
| 1,000 | 10,272.6–10,278.7 ms | 10,171.9–10,205.8 ms | 10,275.3–11,041.7 ms |

Every source map retained the full module inventory, twelve-edit bursts
superseded nine to ten intermediate revisions and compiled the last, and no
main-thread task reached 50 ms. The data is intentionally a failing product signal: the
current full-graph WASM rebuild is not interactive for large graphs and requires
slice invalidation or a faster backend before scale performance is complete.

This is a same-build correctness alarm, not a general browser compatibility or
cross-machine performance guarantee. The macOS release pairs it with Native SDK automation
and Rust-owned artifact and permission tests. The separate Terminal browser
gate exercises the real zsh-to-xterm path during local release qualification.
