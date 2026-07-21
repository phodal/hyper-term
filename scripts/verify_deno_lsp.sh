#!/usr/bin/env bash
set -euo pipefail

verify_repo_root=$(cd "$(dirname "$0")/.." && pwd)
verify_deno=${HYPER_TERM_DENO_PATH:-"$verify_repo_root/.tools/deno/2.9.3/deno"}

for verify_command in awk cargo grep shasum; do
  if ! command -v "$verify_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $verify_command" >&2
    exit 1
  fi
done
if [[ "$verify_deno" != /* ]]; then
  echo "HYPER_TERM_DENO_PATH must be absolute: $verify_deno" >&2
  exit 1
fi
if [[ ! -x "$verify_deno" ]]; then
  echo "verified Deno runtime is unavailable: $verify_deno" >&2
  exit 1
fi
if ! "$verify_deno" --version | grep -q '^deno 2\.9\.3 '; then
  echo "Deno LSP verification requires Deno 2.9.3: $verify_deno" >&2
  exit 1
fi

verify_digest=$(shasum -a 256 "$verify_deno" | awk '{print $1}')
if [[ -n "${HYPER_TERM_DENO_SHA256:-}" && "$verify_digest" != "$HYPER_TERM_DENO_SHA256" ]]; then
  echo "Deno runtime digest does not match HYPER_TERM_DENO_SHA256" >&2
  exit 1
fi
export HYPER_TERM_DENO_PATH="$verify_deno"
export HYPER_TERM_DENO_SHA256="$verify_digest"

cargo test \
  --locked \
  --package hyper-term-drivers \
  --test deno_lsp \
  pinned_deno_lsp_completes_a_real_initialize_handshake \
  -- \
  --ignored \
  --exact

cargo test \
  --locked \
  --package hyper-term-daemon \
  --lib \
  editor_lsp::tests::real_deno_lsp_tracks_draft_diagnostics_and_completion \
  -- \
  --ignored \
  --exact

cargo test \
  --locked \
  --package hyper-term-daemon \
  --lib \
  agent_gateway::tests::authenticated_acp_artifact_editor_queries_the_real_deno_lsp \
  -- \
  --ignored \
  --exact

echo "Deno LSP verified: driver handshake, editor diagnostics and completion, and authenticated ACP Artifact Gateway"
