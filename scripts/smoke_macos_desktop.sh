#!/usr/bin/env bash
set -euo pipefail

smoke_repo_root=$(cd "$(dirname "$0")/.." && pwd)
smoke_supervisor=${1:-"$smoke_repo_root/target/debug/hyper-term-desktop"}
smoke_renderer=${2:-"$smoke_repo_root/apps/desktop/zig-out/bin/hyper-term"}
smoke_acp_fixture="$smoke_repo_root/scripts/fixtures/acp_diff_agent.sh"
smoke_terminal_acp_fixture="$smoke_repo_root/scripts/fixtures/acp_terminal_agent.sh"
smoke_codex_goal_fixture="$smoke_repo_root/scripts/fixtures/codex_goal_agent.sh"
smoke_lima_fixture="$smoke_repo_root/scripts/fixtures/fake_limactl.sh"
smoke_artifact_dir=${HYPER_TERM_SMOKE_ARTIFACT_DIR:-}
smoke_first_frame_budget_ms=${HYPER_TERM_SMOKE_FIRST_FRAME_BUDGET_MS:-750}

if [[ ! "$smoke_first_frame_budget_ms" =~ ^[0-9]+$ ]] || (( smoke_first_frame_budget_ms == 0 )); then
  echo "desktop first-frame budget must be a positive integer in milliseconds" >&2
  exit 1
fi

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
for smoke_command in git native python3 shasum stat; do
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
if [[ ! -x "$smoke_terminal_acp_fixture" ]]; then
  echo "desktop terminal ACP fixture is unavailable: $smoke_terminal_acp_fixture" >&2
  exit 1
fi
if [[ ! -x "$smoke_codex_goal_fixture" ]]; then
  echo "desktop Codex Goal fixture is unavailable: $smoke_codex_goal_fixture" >&2
  exit 1
fi
if [[ ! -x "$smoke_lima_fixture" ]]; then
  echo "desktop Lima fixture is unavailable: $smoke_lima_fixture" >&2
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

smoke_workspace="$smoke_root/workspace"
mkdir -p "$smoke_workspace"
git -C "$smoke_workspace" init -q
git -C "$smoke_workspace" config user.name "Hyper Term Smoke"
git -C "$smoke_workspace" config user.email "hyper-term@example.invalid"
printf 'Hyper Term\n' > "$smoke_workspace/README.md"
git -C "$smoke_workspace" add README.md
git -C "$smoke_workspace" commit -qm fixture

smoke_lima="$smoke_root/limactl"
smoke_lima_image="$smoke_root/tier2.qcow2"
smoke_acp="$smoke_root/acp-diff-agent"
smoke_terminal_acp="$smoke_root/acp-terminal-agent"
smoke_codex_goal="$smoke_root/codex-goal-agent"
cp "$smoke_acp_fixture" "$smoke_acp"
chmod 700 "$smoke_acp"
cp "$smoke_terminal_acp_fixture" "$smoke_terminal_acp"
chmod 700 "$smoke_terminal_acp"
cp "$smoke_codex_goal_fixture" "$smoke_codex_goal"
chmod 700 "$smoke_codex_goal"
cp "$smoke_lima_fixture" "$smoke_lima"
chmod 700 "$smoke_lima"
printf 'local pinned smoke image' > "$smoke_lima_image"
smoke_lima_image_sha256=$(shasum -a 256 "$smoke_lima_image" | awk '{print $1}')

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
    --state-dir "$smoke_root/state" \
    --terminal-assets "$smoke_repo_root/dist/terminal" \
    --workbench-assets "$smoke_repo_root/dist/workbench" \
    --shell-cwd "$smoke_workspace" \
    --codex "$smoke_codex_goal" \
    --codex-acp "$smoke_acp" \
    --claude-agent-acp "$smoke_terminal_acp" \
    --claude "$smoke_terminal_acp" \
    --lima "$smoke_lima" \
    --lima-image "$smoke_lima_image" \
    --lima-image-sha256 "$smoke_lima_image_sha256"
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
  python3 - .zig-cache/native-sdk-automation/snapshot.txt "$smoke_first_frame_budget_ms" <<'PY'
import pathlib
import re
import sys

snapshot = pathlib.Path(sys.argv[1]).read_text()
budget_ms = int(sys.argv[2])
match = re.search(r"\bgpu_first_frame_latency_ns=(\d+)\b", snapshot)
if match is None:
    raise SystemExit("Native snapshot is missing first-frame latency")
latency_ns = int(match.group(1))
cold_start_budget_ns = budget_ms * 1_000_000
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

  native automate shortcut hyper-term.new-codex-agent
  native automate assert \
    'role=group name="Agent conversation"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  smoke_composer_id=$(smoke_widget_id 'role=textbox name="Agent prompt".*enabled=true')
  native automate widget-action hyper-term-canvas "$smoke_composer_id" set-text '/goal Ship the compact Agent UI'
  smoke_send_id=$(smoke_widget_id 'role=button name="Send prompt".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_send_id"
  native automate assert \
    'role=group name="Persistent Agent goal"' \
    'role=button name="Goal actions".*enabled=true' \
    'Goal · Ship the compact Agent UI' \
    'active · 1m · 1200 / 50000 tokens'
  smoke_goal_actions_id=$(smoke_widget_id 'role=button name="Goal actions".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_goal_actions_id"
  native automate assert \
    'role=menuitem name="Edit goal".*enabled=true' \
    'role=menuitem name="Pause goal".*enabled=true' \
    'role=menuitem name="Clear goal".*enabled=true'
  smoke_edit_goal_id=$(smoke_widget_id 'role=menuitem name="Edit goal".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_edit_goal_id"
  native automate assert \
    'role=textbox name="Agent prompt".*focused=true' \
    'role=textbox name="Agent prompt".*text="/goal Ship the compact Agent UI"'
  smoke_goal_actions_id=$(smoke_widget_id 'role=button name="Goal actions".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_goal_actions_id"
  smoke_pause_goal_id=$(smoke_widget_id 'role=menuitem name="Pause goal".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_pause_goal_id"
  native automate assert \
    'role=button name="Goal actions".*enabled=true' \
    'paused · 1m · 1200 / 50000 tokens'
  smoke_goal_actions_id=$(smoke_widget_id 'role=button name="Goal actions".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_goal_actions_id"
  native automate assert \
    'role=menuitem name="Resume goal".*enabled=true' \
    'role=menuitem name="Clear goal".*enabled=true'
  native automate screenshot hyper-term-canvas
  cp "$smoke_screenshot" .zig-cache/native-sdk-automation/screenshot-hyper-term-goal.png
  native automate widget-key hyper-term-canvas cmd+w
  native automate assert --absent \
    'role=group name="Persistent Agent goal"' \
    'role=menuitem name="Resume goal"' \
    'error event=' \
    'dispatch_errors=[1-9]'

  native automate shortcut hyper-term.new-codex-acp-agent
  native automate assert \
    'role=group name="Agent conversation"' \
    'role=group name="Agent reading rail"' \
    'role=group name="Agent composer rail"' \
    'role=group name="Agent prompt composer"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Inspect Agent execution context"' \
    'role=button name="Send prompt".*enabled=true'
  native automate assert --absent \
    'name="ACP artifact editor"' \
    'name="Agent execution context details"' \
    'error event=' \
    'dispatch_errors=[1-9]'

  smoke_context_id=$(smoke_widget_id 'role=button name="Inspect Agent execution context"')
  native automate widget-click hyper-term-canvas "$smoke_context_id"
  native automate assert \
    'role=group name="Agent execution context details"' \
    'agent:codex-acp:' \
    'Hermetic' \
    'credential references'
  native automate widget-click hyper-term-canvas "$smoke_context_id"
  native automate assert --absent 'name="Agent execution context details"'

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

  native automate shortcut hyper-term.new-claude-acp-agent
  native automate assert \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  smoke_composer_id=$(smoke_widget_id 'role=textbox name="Agent prompt".*enabled=true')
  native automate widget-action hyper-term-canvas "$smoke_composer_id" set-text 'Run the bounded terminal'
  native automate assert \
    'role=textbox name="Agent prompt".*text="Run the bounded terminal"' \
    'role=button name="Send prompt".*enabled=true'
  smoke_send_id=$(smoke_widget_id 'role=button name="Send prompt".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_send_id"
  native automate assert \
    'Approval required' \
    'Shell command · external effect' \
    'Isolated Tier 2 command · no ordinary PTY access' \
    'name="Agent turn plan"' \
    'role=button name="Plan · Run the isolated terminal"' \
    '0 / 2' \
    'role=button name="Allow once".*enabled=true'
  smoke_allow_id=$(smoke_widget_id 'role=button name="Allow once".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_allow_id"
  native automate assert \
    'Tier 2 terminal completed.' \
    'role=button name="Plan complete"' \
    '2 / 2' \
    'role=button name="Allowed once"'
  native automate assert --absent 'Decision: allowed once'
  smoke_receipt_id=$(smoke_widget_id 'role=button name="Allowed once"')
  native automate widget-click hyper-term-canvas "$smoke_receipt_id"
  native automate assert 'Decision: allowed once'
  native automate widget-click hyper-term-canvas "$smoke_receipt_id"
  native automate assert --absent 'Decision: allowed once'
  smoke_plan_id=$(smoke_widget_id 'role=button name="Plan complete"')
  native automate widget-click hyper-term-canvas "$smoke_plan_id"
  native automate assert \
    'Run the isolated terminal' \
    'Review the retained result'
  native automate widget-click hyper-term-canvas "$smoke_plan_id"
  native automate assert --absent 'Review the retained result'
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
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-goal.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-goal.png"
  cp "$smoke_log" "$smoke_artifact_dir/hyper-term-smoke.log"
fi

echo "Hyper Term macOS desktop smoke passed"
