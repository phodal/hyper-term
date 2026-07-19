# Third-party notices

## OpenAI Codex macOS Seatbelt base policy

`crates/hyper-term-sandbox/src/seatbelt_base_policy.sbpl` and
`crates/hyper-term-sandbox/src/seatbelt_network_policy.sbpl` are derived from
the OpenAI Codex Seatbelt policies at `codex-rs/sandboxing/src`. The derived
base file removes Codex's broad process execution, temporary-directory write,
and network rules so Hyper Term can compile operation-bound executable and
filesystem rules. The network file contains only the platform services needed
alongside an exact Rust-owned loopback proxy port; it does not grant general
outbound, DNS, inbound, or Unix-socket access.

OpenAI Codex is licensed under the Apache License, Version 2.0. The repository
is available at <https://github.com/openai/codex>.

Copyright 2025 OpenAI
