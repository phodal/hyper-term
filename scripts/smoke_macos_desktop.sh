#!/usr/bin/env bash
set -euo pipefail

smoke_repo_root=$(cd "$(dirname "$0")/.." && pwd)
smoke_supervisor=${1:-"$smoke_repo_root/target/debug/hyper-term-desktop"}
smoke_renderer=${2:-"$smoke_repo_root/apps/desktop/zig-out/bin/hyper-term"}
smoke_acp_fixture="$smoke_repo_root/scripts/fixtures/acp_diff_agent.sh"
smoke_artifact_dir=${HYPER_TERM_SMOKE_ARTIFACT_DIR:-}

if [[ "$smoke_supervisor" != /* ]]; then
  smoke_supervisor="$PWD/$smoke_supervisor"
fi
if [[ "$smoke_renderer" != /* ]]; then
  smoke_renderer="$PWD/$smoke_renderer"
fi
if [[ -n "$smoke_artifact_dir" && "$smoke_artifact_dir" != /* ]]; then
  smoke_artifact_dir="$PWD/$smoke_artifact_dir"
fi

if [[ $(uname -s) != Darwin ]]; then
  echo "macOS desktop smoke requires a macOS host" >&2
  exit 1
fi
for smoke_command in native python3 stat; do
  if ! command -v "$smoke_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $smoke_command" >&2
    exit 1
  fi
done
if [[ ! -x "$smoke_supervisor" ]]; then
  echo "desktop supervisor is unavailable: $smoke_supervisor" >&2
  exit 1
fi
if [[ ! -x "$smoke_renderer" ]]; then
  echo "automation-enabled Native renderer is unavailable: $smoke_renderer" >&2
  exit 1
fi
if [[ ! -x "$smoke_acp_fixture" ]]; then
  echo "desktop ACP fixture is unavailable: $smoke_acp_fixture" >&2
  exit 1
fi
for smoke_asset in \
  "$smoke_repo_root/dist/terminal/index.html" \
  "$smoke_repo_root/dist/workbench/index.html"; do
  if [[ ! -f "$smoke_asset" ]]; then
    echo "built desktop asset is unavailable: $smoke_asset" >&2
    exit 1
  fi
done

smoke_root=$(mktemp -d)
smoke_log="$smoke_root/hyper-term-smoke.log"
smoke_pid=""

smoke_cleanup() {
  smoke_status=$?
  trap - EXIT INT TERM
  if [[ -n "$smoke_pid" ]] && kill -0 "$smoke_pid" 2>/dev/null; then
    kill -INT "$smoke_pid" 2>/dev/null || true
    wait "$smoke_pid" 2>/dev/null || true
  fi
  if [[ $smoke_status -ne 0 ]]; then
    echo "Hyper Term desktop smoke failed; supervisor log follows:" >&2
    tail -n 80 "$smoke_log" >&2 || true
  fi
  rm -rf "$smoke_root"
  exit "$smoke_status"
}
trap smoke_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

(
  cd "$smoke_root"
  exec "$smoke_supervisor" \
    --ui "$smoke_renderer" \
    --terminal-assets "$smoke_repo_root/dist/terminal" \
    --workbench-assets "$smoke_repo_root/dist/workbench" \
    --codex "$smoke_acp_fixture" \
    --codex-acp "$smoke_acp_fixture"
) >"$smoke_log" 2>&1 &
smoke_pid=$!

(
  cd "$smoke_root"
  native automate wait
  native automate assert \
    'ready=true' \
    'gpu_nonblank=true' \
    'canvas_frame_budget_ok=true' \
    'role=button name="New Terminal tab"' \
    'role=button name="New Agent tab"' \
    'role=button name="Close zsh 1"' \
    'hyper-term-terminal-view.*url="http://127.0.0.1:47437/.*tab=1"'
  native automate assert --absent 'error event=' 'dispatch_errors=[1-9]'
  python3 - .zig-cache/native-sdk-automation/snapshot.txt <<'PY'
import pathlib
import re
import sys

snapshot = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r"\bgpu_first_frame_latency_ns=(\d+)\b", snapshot)
if match is None:
    raise SystemExit("Native snapshot is missing first-frame latency")
latency_ns = int(match.group(1))
cold_start_budget_ns = 750_000_000
if latency_ns > cold_start_budget_ns:
    raise SystemExit(
        f"Native cold first frame took {latency_ns / 1_000_000:.1f} ms; "
        f"budget is {cold_start_budget_ns / 1_000_000:.0f} ms"
    )
print(f"Native cold first frame: {latency_ns / 1_000_000:.1f} ms")
PY

  native automate widget-key hyper-term-canvas cmd+t
  native automate assert \
    'role=button name="Close zsh 1"' \
    'role=button name="Close zsh 2"' \
    'hyper-term-terminal-view.*tab=2"'

  native automate widget-key hyper-term-canvas cmd+w
  native automate assert --absent \
    'role=button name="Close zsh 2"' \
    'error event=' \
    'dispatch_errors=[1-9]'
  native automate assert \
    'role=button name="Close zsh 1"' \
    'hyper-term-terminal-view.*tab=1"'

  native automate screenshot hyper-term-canvas
  smoke_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png
  if [[ ! -s "$smoke_screenshot" ]]; then
    echo "Native renderer screenshot is empty" >&2
    exit 1
  fi
  smoke_screenshot_bytes=$(stat -f '%z' "$smoke_screenshot")
  if (( smoke_screenshot_bytes < 100000 )); then
    echo "Native renderer screenshot is unexpectedly small: $smoke_screenshot_bytes bytes" >&2
    exit 1
  fi
  smoke_terminal_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-terminal.png
  cp "$smoke_screenshot" "$smoke_terminal_screenshot"

  native automate shortcut hyper-term.new-codex-acp-agent
  native automate assert \
    'role=group name="Agent conversation"' \
    'role=group name="Agent reading rail"' \
    'role=group name="Agent composer rail"' \
    'role=group name="Agent prompt composer"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  native automate assert --absent \
    'name="ACP artifact editor"' \
    'error event=' \
    'dispatch_errors=[1-9]'

  smoke_widget_id() {
    python3 - .zig-cache/native-sdk-automation/snapshot.txt "$1" <<'PY'
import pathlib
import re
import sys

snapshot = pathlib.Path(sys.argv[1]).read_text()
pattern = re.compile(sys.argv[2])
for line in snapshot.splitlines():
    if pattern.search(line) is None:
        continue
    match = re.search(r"#(\d+)", line)
    if match is not None:
        print(match.group(1))
        raise SystemExit(0)
raise SystemExit(f"widget not found: {pattern.pattern}")
PY
  }

  smoke_composer_id=$(smoke_widget_id 'role=textbox name="Agent prompt".*enabled=true')
  native automate widget-action hyper-term-canvas "$smoke_composer_id" set-text 'Show the file change.'
  native automate assert \
    'role=textbox name="Agent prompt".*text="Show the file change\."' \
    'role=button name="Send prompt".*enabled=true'
  smoke_send_id=$(smoke_widget_id 'role=button name="Send prompt".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_send_id"
  native automate assert \
    'role=button name="Processed"' \
    'The proposed file change is ready to review.'
  smoke_activity_id=$(smoke_widget_id 'role=button name="Processed"')
  native automate widget-click hyper-term-canvas "$smoke_activity_id"
  native automate assert \
    'name="Changed files"' \
    'name="Changed file README.md, plus 1, minus 0"' \
    'AI Terminal'
  native automate assert --absent 'error event=' 'dispatch_errors=[1-9]'
  native automate screenshot hyper-term-canvas
  smoke_agent_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-agent.png
  cp "$smoke_screenshot" "$smoke_agent_screenshot"
  cp "$smoke_terminal_screenshot" "$smoke_screenshot"
)

if [[ -n "$smoke_artifact_dir" ]]; then
  mkdir -p "$smoke_artifact_dir"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/snapshot.txt" \
    "$smoke_artifact_dir/snapshot.txt"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-canvas.png"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-terminal.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-terminal.png"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-agent.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-agent.png"
  cp "$smoke_log" "$smoke_artifact_dir/hyper-term-smoke.log"
fi

echo "Hyper Term macOS desktop smoke passed"
