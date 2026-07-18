# Terminal core release baseline — 2026-07-18

This is a local regression baseline for the Rust PTY path. It is not yet a
competitive terminal-emulator benchmark: glyph shaping, renderer present time,
scrollback search, IME, GPU frame pacing, and comparisons with Terminal,
Ghostty, Warp, or other products still require the real macOS application.

## Environment

- macOS 26.5.2 (25F84), Apple Silicon (`aarch64`)
- Rust 1.95.0
- user login shell `/bin/zsh`, zsh 5.9, including the real user startup files
- release profile with thin LTO and one codegen unit

## Workloads

Run the machine-readable probe with:

```bash
cargo run --release -p hyper-term-core --bin terminal-probe -- --assert-budget
```

The probe measures four Rust-owned paths:

1. PTY spawn through execution of a marker in the real user login shell;
2. 64 sequential input writes to the first observed PTY echo;
3. an 8 MiB binary burst through the PTY reader and ordered replay buffer;
4. 1,000 sequential PTY resize operations.

Three warm release runs produced:

| Measurement | Observed range | Initial local budget |
| --- | ---: | ---: |
| zsh startup to executed marker | 185.49–188.08 ms | ≤ 1,000 ms |
| key-to-PTY-echo p95 | 0.032–0.037 ms | ≤ 5 ms |
| 8 MiB PTY burst | 87.19–87.59 MiB/s | ≥ 75 MiB/s |
| 1,000 resize operations | 2.15–2.27 ms | ≤ 100 ms |
| ordered output chunks for 8 MiB | 594–673 | informational |

The burst coalescer uses a zero-timeout readiness check. It reduced the local
8 MiB workload from 8,192 roughly 1 KiB publications to about 600 ordered
chunks without imposing a batching delay on isolated interactive reads.

## Interpretation and next gate

These budgets are intentionally conservative same-machine regression alarms.
They must not be presented as cross-machine or competitor results. The desktop
gate must extend the same workload from input through native render and display
present, then run identical corpus, window, font, shell, and hardware settings
against the macOS Terminal baseline and selected competitors. The Agent/Block
pipeline must be disabled for the default Terminal run and must never sit on
the input-to-present hot path.
