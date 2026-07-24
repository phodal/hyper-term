#!/usr/bin/env bash
set -euo pipefail

# Developer-only packaged-app gate for the direct Codex app-server path. The
# shared runner also covers ACP providers, but this entry point keeps the direct
# path visible in release checklists and local audit commands.
real_codex_repo_root=$(cd "$(dirname "$0")/.." && pwd)
export HYPER_TERM_REAL_ACP_PROVIDER=codex_direct
exec "$real_codex_repo_root/scripts/smoke_macos_real_codex_acp.sh" "$@"
