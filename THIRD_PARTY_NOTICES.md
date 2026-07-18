# Third-party notices

## OpenAI Codex macOS Seatbelt base policy

`crates/hyper-term-sandbox/src/seatbelt_base_policy.sbpl` is derived from the
OpenAI Codex Seatbelt base policy at
`codex-rs/sandboxing/src/seatbelt_base_policy.sbpl` and its restricted read-only
platform defaults. The derived file removes Codex's broad process execution,
temporary-directory write, and network rules so Hyper Term can compile
operation-bound executable and filesystem rules.

OpenAI Codex is licensed under the Apache License, Version 2.0. The repository
is available at <https://github.com/openai/codex>.

Copyright 2025 OpenAI
