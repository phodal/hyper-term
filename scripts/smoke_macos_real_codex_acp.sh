#!/usr/bin/env bash
set -euo pipefail

# Opt-in release confidence gate. This deliberately talks to the user's real,
# authenticated Codex account, so it is never part of unattended CI.

real_repo_root=$(cd "$(dirname "$0")/.." && pwd)
real_app=${1:-"$real_repo_root/dist/macos/Hyper Term.app"}
real_renderer=${2:-"$real_repo_root/apps/desktop/zig-out/bin/hyper-term"}
real_expected=${HYPER_TERM_REAL_ACP_EXPECTED_TEXT:-HYPER_TERM_REAL_DESKTOP_ACP_OK}
real_artifact_dir=${HYPER_TERM_REAL_ACP_ARTIFACT_DIR:-}

if [[ "$real_app" != /* ]]; then
  real_app="$PWD/$real_app"
fi
if [[ "$real_renderer" != /* ]]; then
  real_renderer="$PWD/$real_renderer"
fi
if [[ ! "$real_expected" =~ ^[A-Za-z0-9_:-]{1,64}$ ]]; then
  echo "HYPER_TERM_REAL_ACP_EXPECTED_TEXT must be a 1-64 byte marker" >&2
  exit 1
fi

if [[ $(uname -s) != Darwin ]]; then
  echo "real Codex ACP desktop smoke requires macOS" >&2
  exit 1
fi

for real_command in codex grep native sed tail; do
  if ! command -v "$real_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $real_command" >&2
    exit 1
  fi
done

real_codex=$(command -v codex)
if ! "$real_codex" login status 2>&1 | grep -q '^Logged in'; then
  echo "Codex is not authenticated; run 'codex login' before this opt-in smoke" >&2
  exit 1
fi

real_supervisor="$real_app/Contents/MacOS/hyper-term"
real_runtime="$real_app/Contents/Resources/runtime"
real_adapter="$real_runtime/acp/node_modules/@agentclientprotocol/codex-acp/dist/index.js"
for real_path in \
  "$real_supervisor" \
  "$real_renderer" \
  "$real_runtime/deno" \
  "$real_adapter" \
  "$real_repo_root/dist/terminal/index.html" \
  "$real_repo_root/dist/workbench/index.html"; do
  if [[ ! -e "$real_path" ]]; then
    echo "real Codex ACP desktop input is unavailable: $real_path" >&2
    exit 1
  fi
done

if [[ -n "$real_artifact_dir" && "$real_artifact_dir" != /* ]]; then
  real_artifact_dir="$PWD/$real_artifact_dir"
fi

real_root=$(mktemp -d /tmp/hyper-term-real-codex-acp.XXXXXX)
real_log="$real_root/hyper-term-real-codex-acp.log"
real_pid=""

real_cleanup() {
  real_status=$?
  trap - EXIT INT TERM
  if [[ -n "$real_pid" ]] && kill -0 "$real_pid" 2>/dev/null; then
    kill -INT "$real_pid" 2>/dev/null || true
    wait "$real_pid" 2>/dev/null || true
  fi
  if [[ $real_status -ne 0 ]]; then
    echo "real Codex ACP desktop smoke failed; supervisor log follows:" >&2
    tail -n 100 "$real_log" >&2 || true
  fi
  rm -rf "$real_root"
  exit "$real_status"
}
trap real_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

real_widget_id() {
  real_pattern=$1
  native automate snapshot |
    sed -n "s/.*#\([0-9][0-9]*\) $real_pattern.*/\1/p" |
    tail -n 1
}

(
  cd "$real_root"
  exec "$real_supervisor" \
    --ui "$real_renderer" \
    --state-dir "$real_root/state" \
    --terminal-assets "$real_repo_root/dist/terminal" \
    --workbench-assets "$real_repo_root/dist/workbench" \
    --shell-cwd "$real_repo_root" \
    --codex "$real_codex"
) >"$real_log" 2>&1 &
real_pid=$!

(
  cd "$real_root"
  native automate wait
  native automate assert \
    'ready=true' \
    'gpu_nonblank=true' \
    'role=button name="New Agent tab"'
  native automate shortcut hyper-term.new-codex-acp-agent
  native automate assert --timeout-ms 30000 \
    'Codex ACP' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'

  real_composer_id=$(real_widget_id 'role=textbox name="Agent prompt".*enabled=true')
  if [[ -z "$real_composer_id" ]]; then
    echo "real Codex ACP composer widget is unavailable" >&2
    exit 1
  fi
  native automate widget-action hyper-term-canvas "$real_composer_id" set-text \
    "Reply with exactly $real_expected. Do not use tools or modify files."
  native automate assert \
    "role=textbox name=\"Agent prompt\".*$real_expected" \
    'role=button name="Send prompt".*enabled=true'

  real_send_id=$(real_widget_id 'role=button name="Send prompt".*enabled=true')
  if [[ -z "$real_send_id" ]]; then
    echo "real Codex ACP send widget is unavailable" >&2
    exit 1
  fi
  native automate widget-click hyper-term-canvas "$real_send_id"
  native automate assert --timeout-ms 30000 'role=button name="Stop Agent turn"'
  native automate assert --timeout-ms 150000 \
    "$real_expected" \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  native automate assert --absent \
    'role=button name="Stop Agent turn"' \
    'error event=' \
    'dispatch_errors=[1-9]'
  native automate screenshot hyper-term-canvas
)

if [[ -n "$real_artifact_dir" ]]; then
  mkdir -p "$real_artifact_dir"
  cp \
    "$real_root/.zig-cache/native-sdk-automation/snapshot.txt" \
    "$real_artifact_dir/snapshot.txt"
  cp \
    "$real_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png" \
    "$real_artifact_dir/screenshot-hyper-term-real-codex-acp.png"
  cp "$real_log" "$real_artifact_dir/hyper-term-real-codex-acp.log"
fi

echo "real Codex ACP desktop smoke passed: $real_expected"
