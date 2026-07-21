# Terminal browser release baseline — 2026-07-21

This is the first end-to-end regression baseline for the built Terminal WebView.
It extends the Rust PTY baseline through the authenticated loopback gateway,
ordered WebSocket protocol, xterm parser, and WebGL renderer. It is a
same-machine release alarm, not a competitor benchmark or key-to-present
measurement.

## Environment

- macOS 26.5.2 (25F84), Apple M4 Max (`arm64`)
- Rust 1.95.0, Deno 2.9.3, Zig 0.16.0
- agent-browser 0.25.4 using Chromium
- real `/bin/zsh` PTY through `hyperd`
- xterm 6.0.0 with the WebGL renderer
- screen-reader mode disabled for the burst workload

## Reproduce

Build the two release-probe inputs, then run the browser gate:

```bash
deno task build:terminal
cargo build -p hyper-term-daemon --bin hyperd
deno task verify:terminal-browser
```

The script starts `hyperd` on an ephemeral loopback port with a private token,
loads the exact built assets, and checks ordinary input, CJK IME, selection,
Command-F focus, opt-in accessibility, and resize before the burst. The burst
command writes 8 MiB through the real shell and then prints a completion marker.
The diagnostic query mode exports only counters and monotonic timestamps; it
never exposes PTY bytes, cells, selections, or scrollback content.

## Initial result

One warm run after two browser viewport resizes produced:

| Measurement                     |  Observed | Initial local budget |
| ------------------------------- | --------: | -------------------: |
| Received output bytes           | 8,647,110 |       at least 8 MiB |
| Ordered output frames           |       631 |          more than 1 |
| xterm render events             |        17 |          more than 0 |
| Command-to-last-output duration |    583 ms |    at most 10,000 ms |
| Browser page errors             |         0 |                    0 |

The duration begins immediately before browser input synthesis and ends when the
final output frame reaches the page. It includes CLI automation overhead, shell
parsing, PTY I/O, WebSocket transport, and browser event-loop scheduling; it
does not prove that the final glyph was presented on screen at that exact
timestamp. The retained screenshot provides the complementary visual evidence:
the WebGL grid shows the burst tail, completion marker, prompt, cursor, and
scrollbar after resize.

## Next gate

Add a macOS system-WebView frame-present timestamp and compare the same corpus,
window, font, and hardware against Terminal, Ghostty, Warp, and selected Rust
terminals. Track p50/p95 key-to-present, CPU/GPU time, RSS, long main-thread
tasks, and dropped frames. Alternate-screen programs, hyperlinks, 100k/1M-line
scrollback, and renderer crash recovery remain separate correctness gates.
