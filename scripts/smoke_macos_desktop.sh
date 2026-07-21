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
smoke_first_frame_budget_ms=${HYPER_TERM_SMOKE_FIRST_FRAME_BUDGET_MS:-150}

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
for smoke_command in git native pgrep python3 shasum stat; do
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

smoke_start_supervisor() {
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
  ) >>"$smoke_log" 2>&1 &
  smoke_pid=$!
}

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

smoke_start_supervisor

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
    'hyper-term-terminal-view.*focused=true.*url="http://127.0.0.1:47437/.*tab=1"'
  native automate assert --absent \
    'view @w1/hyper-term-genui-view' \
    'error event=' \
    'dispatch_errors=[1-9]'
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
    'hyper-term-terminal-view.*focused=true.*tab=2"'

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

  smoke_terminal_url() {
    python3 - .zig-cache/native-sdk-automation/snapshot.txt <<'PY'
import pathlib
import re
import sys

snapshot = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'hyper-term-terminal-view.*url="([^"]+)"', snapshot)
if match is None:
    raise SystemExit("terminal WebView URL is missing from Native snapshot")
print(match.group(1))
PY
  }

  smoke_terminal_url_before_restart=$(smoke_terminal_url)
  native automate widget-key hyper-term-canvas cmd+t
  native automate shortcut hyper-term.new-codex-agent
  native automate assert \
    'role=button name="Close zsh 1"' \
    'role=button name="Close zsh 3"' \
    'role=button name="Close Codex 4"' \
    'role=group name="Agent conversation"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  python3 - "$smoke_terminal_url_before_restart" .zig-cache/native-sdk-automation/workspace-before-restart.json <<'PY'
import json
import pathlib
import sys
import time
import urllib.parse
import urllib.request

terminal_url = urllib.parse.urlsplit(sys.argv[1])
token = urllib.parse.parse_qs(terminal_url.query)["token"][0]
workspace_url = urllib.parse.urlunsplit(
    (terminal_url.scheme, terminal_url.netloc, "/desktop/workspace", f"token={token}", "")
)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
last = None
for _ in range(100):
    with opener.open(workspace_url, timeout=2) as response:
        last = json.load(response)
    sessions = [
        (entry["id"], entry["mode"], entry.get("agent_provider"))
        for entry in last.get("sessions", [])
    ]
    if (
        last.get("active_session_id") == 4
        and sessions
        == [
            (1, "terminal", None),
            (3, "terminal", None),
            (4, "agent", "codex"),
        ]
    ):
        pathlib.Path(sys.argv[2]).write_text(json.dumps(last, sort_keys=True) + "\n")
        break
    time.sleep(0.05)
else:
    raise SystemExit(f"Rust workspace did not retain the desktop tabs: {last!r}")
PY
  smoke_renderer_pid=$(pgrep -P "$smoke_pid" -f "$smoke_renderer" | head -n 1)
  if [[ ! "$smoke_renderer_pid" =~ ^[0-9]+$ ]]; then
    echo "cannot resolve supervised Native renderer pid" >&2
    exit 1
  fi
  kill -KILL "$smoke_renderer_pid"
  smoke_restarted_renderer_pid=""
  for _ in {1..100}; do
    smoke_restarted_renderer_pid=$(pgrep -P "$smoke_pid" -f "$smoke_renderer" | head -n 1 || true)
    if [[ "$smoke_restarted_renderer_pid" =~ ^[0-9]+$ ]] &&
      [[ "$smoke_restarted_renderer_pid" != "$smoke_renderer_pid" ]]; then
      break
    fi
    sleep 0.05
  done
  if [[ ! "$smoke_restarted_renderer_pid" =~ ^[0-9]+$ ]] ||
    [[ "$smoke_restarted_renderer_pid" == "$smoke_renderer_pid" ]]; then
    echo "Rust supervisor did not restart the crashed Native renderer" >&2
    exit 1
  fi
  if ! kill -0 "$smoke_pid" 2>/dev/null; then
    echo "Rust desktop supervisor exited with the crashed renderer" >&2
    exit 1
  fi
  native automate assert --timeout-ms 30000 \
    "publisher_pid=$smoke_restarted_renderer_pid" \
    'ready=true' \
    'gpu_nonblank=true' \
    'role=button name="Close zsh 1"' \
    'role=button name="Close zsh 3"' \
    'role=button name="Close Codex 4"' \
    'role=group name="Agent conversation"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  smoke_terminal_tab_id=$(smoke_widget_id 'role=button name="zsh 3"')
  native automate widget-click hyper-term-canvas "$smoke_terminal_tab_id"
  native automate assert \
    'role=button name="zsh 3".*state=.*selected' \
    'hyper-term-terminal-view.*focused=true.*tab=3"'
  smoke_terminal_url_after_restart=$(smoke_terminal_url)
  if [[ "${smoke_terminal_url_after_restart%&tab=*}" != "${smoke_terminal_url_before_restart%&tab=*}" ]]; then
    echo "renderer restart replaced the Rust terminal gateway identity" >&2
    exit 1
  fi
  smoke_agent_tab_id=$(smoke_widget_id 'role=button name="Codex 4"')
  native automate widget-click hyper-term-canvas "$smoke_agent_tab_id"
  native automate assert \
    'role=button name="Codex 4".*state=.*selected' \
    'role=group name="Agent conversation"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  if ! grep -Fq 'native renderer exited with signal: 9' "$smoke_log"; then
    echo "Rust supervisor did not record the bounded renderer restart" >&2
    exit 1
  fi
  smoke_restart_evidence=.zig-cache/native-sdk-automation/renderer-restart.txt
  printf '%s\n' \
    "supervisor_pid=$smoke_pid" \
    "renderer_before=$smoke_renderer_pid" \
    "renderer_after=$smoke_restarted_renderer_pid" \
    'terminal_gateway_identity_preserved=true' \
    'workspace_tabs_restored=true' \
    'agent_session_rebound=true' \
    > "$smoke_restart_evidence"

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
    'view @w1/hyper-term-genui-view' \
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
  native automate widget-action hyper-term-canvas "$smoke_composer_id" set-composition '中文输入'
  native automate assert \
    'role=textbox name="Agent prompt".*text="中文输入"' \
    'role=textbox name="Agent prompt".*composition=0\.\.12'
  native automate widget-action hyper-term-canvas "$smoke_composer_id" commit-composition
  native automate assert 'role=textbox name="Agent prompt".*text="中文输入"'
  native automate assert --absent 'role=textbox name="Agent prompt".*composition='
  native automate widget-action hyper-term-canvas "$smoke_composer_id" set-text 'Show the file change.'
  native automate assert \
    'role=textbox name="Agent prompt".*text="Show the file change\."' \
    'role=button name="Send prompt".*enabled=true'
  smoke_send_id=$(smoke_widget_id 'role=button name="Send prompt".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_send_id"
  native automate assert \
    'role=button name="Processed"' \
    'The proposed file change is ready to review.' \
    'role=textbox name="Agent prompt".*focused=true'
  smoke_activity_id=$(smoke_widget_id 'role=button name="Processed"')
  native automate widget-click hyper-term-canvas "$smoke_activity_id"
  native automate assert \
    'name="Changed files"' \
    'name="Changed file README.md, plus 1, minus 0"' \
    'AI Terminal'
  native automate assert --absent 'error event=' 'dispatch_errors=[1-9]'

  native automate widget-key hyper-term-canvas cmd+f
  native automate assert \
    'role=group name="Agent history search"' \
    'role=textbox name="Search Agent history".*focused=true' \
    'role=button name="Close Agent history search"'
  smoke_search_id=$(smoke_widget_id 'role=textbox name="Search Agent history"')
  native automate widget-action hyper-term-canvas "$smoke_search_id" set-text 'README'
  native automate assert \
    'role=textbox name="Search Agent history".*text="README"' \
    '1 matches' \
    'name="Changed file README.md, plus 1, minus 0"'
  native automate assert --absent \
    'The proposed file change is ready to review\.' \
    'error event=' \
    'dispatch_errors=[1-9]'
  native automate screenshot hyper-term-canvas
  smoke_search_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-search.png
  cp "$smoke_screenshot" "$smoke_search_screenshot"
  smoke_close_search_id=$(smoke_widget_id 'role=button name="Close Agent history search"')
  native automate widget-click hyper-term-canvas "$smoke_close_search_id"
  native automate assert --absent \
    'role=group name="Agent history search"' \
    'role=textbox name="Search Agent history"'
  native automate assert \
    'The proposed file change is ready to review\.' \
    'role=textbox name="Agent prompt".*focused=true'

  native automate shortcut hyper-term.new-claude-acp-agent
  native automate assert \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  native automate assert --timeout-ms 30000 \
    'role=group name="Codex ACP tab [0-9]+, review ready"'
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

  smoke_terminal_tab_id=$(smoke_widget_id 'role=button name="zsh 3"')
  native automate widget-click hyper-term-canvas "$smoke_terminal_tab_id"
  native automate assert \
    'role=button name="zsh 3".*state=.*selected' \
    'hyper-term-terminal-view.*tab=3"'
  smoke_terminal_url_before_application_restart=$(smoke_terminal_url)
  smoke_agent_tab_id=$(smoke_widget_id 'role=button name="Claude ACP 6"')
  native automate widget-click hyper-term-canvas "$smoke_agent_tab_id"
  native automate assert \
    'role=button name="Claude ACP 6".*state=.*selected' \
    'role=group name="Agent conversation"'
  python3 \
    - "$smoke_root/state/desktop-workspace.json" \
    "$smoke_root/state/agent-runtime/agent-session-bindings.json" \
    .zig-cache/native-sdk-automation/workspace-before-application-restart.json \
    .zig-cache/native-sdk-automation/agent-bindings-before-application-restart.json <<'PY'
import json
import pathlib
import stat
import sys
import time

state_path = pathlib.Path(sys.argv[1])
binding_path = pathlib.Path(sys.argv[2])
workspace_evidence_path = pathlib.Path(sys.argv[3])
binding_evidence_path = pathlib.Path(sys.argv[4])
last = None
for _ in range(100):
    try:
        last = json.loads(state_path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        time.sleep(0.05)
        continue
    sessions = [
        (entry["id"], entry["mode"], entry.get("agent_provider"))
        for entry in last.get("sessions", [])
    ]
    if (
        last.get("active_session_id") == 6
        and sessions
        == [
            (1, "terminal", None),
            (3, "terminal", None),
            (5, "agent", "codex-acp"),
            (6, "agent", "claude-acp"),
        ]
    ):
        break
    time.sleep(0.05)
else:
    raise SystemExit(f"durable desktop workspace did not settle: {last!r}")

mode = stat.S_IMODE(state_path.stat().st_mode)
if mode != 0o600:
    raise SystemExit(f"desktop workspace mode is {mode:o}, expected 600")
workspace_evidence_path.write_text(json.dumps(last, sort_keys=True) + "\n")

bindings = None
for _ in range(100):
    try:
        bindings = json.loads(binding_path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        time.sleep(0.05)
        continue
    entries = [
        (entry["session_id"], entry["provider"])
        for entry in bindings.get("entries", [])
    ]
    if entries == [(5, "codex-acp"), (6, "claude-acp")]:
        break
    time.sleep(0.05)
else:
    raise SystemExit(f"durable Agent bindings did not settle: {bindings!r}")

mode = stat.S_IMODE(binding_path.stat().st_mode)
if mode != 0o600:
    raise SystemExit(f"Agent binding mode is {mode:o}, expected 600")
encoded = binding_path.read_text()
for forbidden in ("Run the bounded terminal", "Tier 2 terminal completed", "token"):
    if forbidden in encoded:
        raise SystemExit(f"Agent binding leaked transcript or secret content: {forbidden!r}")
binding_evidence_path.write_text(json.dumps(bindings, sort_keys=True) + "\n")
PY

  smoke_supervisor_before_application_restart=$smoke_pid
  smoke_renderer_before_application_restart=$(pgrep -P "$smoke_pid" -f "$smoke_renderer" | head -n 1)
  kill -INT "$smoke_pid"
  for _ in {1..100}; do
    smoke_supervisor_state=$(ps -o stat= -p "$smoke_pid" 2>/dev/null | tr -d ' ' || true)
    if [[ -z "$smoke_supervisor_state" || "$smoke_supervisor_state" == Z* ]]; then
      break
    fi
    sleep 0.05
  done
  if [[ -n "$smoke_supervisor_state" && "$smoke_supervisor_state" != Z* ]]; then
    echo "desktop supervisor did not stop for the application restart" >&2
    exit 1
  fi

  smoke_start_supervisor
  smoke_supervisor_after_application_restart=$smoke_pid
  smoke_renderer_after_application_restart=""
  for _ in {1..100}; do
    smoke_renderer_after_application_restart=$(pgrep -P "$smoke_pid" -f "$smoke_renderer" | head -n 1 || true)
    if [[ "$smoke_renderer_after_application_restart" =~ ^[0-9]+$ ]]; then
      break
    fi
    sleep 0.05
  done
  if [[ ! "$smoke_renderer_after_application_restart" =~ ^[0-9]+$ ]]; then
    echo "restarted desktop supervisor did not launch the Native renderer" >&2
    exit 1
  fi
  native automate assert --timeout-ms 30000 \
    "publisher_pid=$smoke_renderer_after_application_restart" \
    'ready=true' \
    'gpu_nonblank=true' \
    'role=button name="Close zsh 1"' \
    'role=button name="Close zsh 3"' \
    'role=button name="Close Codex ACP 5"' \
    'role=button name="Close Claude ACP 6"' \
    'role=button name="Claude ACP 6".*state=.*selected' \
    'role=group name="Agent conversation"' \
    'role=group name="Recovered Agent history"' \
    'History restored' \
    'Run the bounded terminal' \
    'Tier 2 terminal completed\.' \
    'role=button name="Allowed once"' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  native automate screenshot hyper-term-canvas
  smoke_restored_agent_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-agent-restored.png
  cp "$smoke_screenshot" "$smoke_restored_agent_screenshot"

  smoke_terminal_tab_id=$(smoke_widget_id 'role=button name="zsh 3"')
  native automate widget-click hyper-term-canvas "$smoke_terminal_tab_id"
  native automate assert \
    'role=button name="zsh 3".*state=.*selected' \
    'hyper-term-terminal-view.*tab=3"'
  smoke_terminal_url_after_application_restart=$(smoke_terminal_url)
  if [[ "$smoke_terminal_url_after_application_restart" == "$smoke_terminal_url_before_application_restart" ]]; then
    echo "application restart reused the previous terminal gateway token" >&2
    exit 1
  fi
  smoke_agent_tab_id=$(smoke_widget_id 'role=button name="Claude ACP 6"')
  native automate widget-click hyper-term-canvas "$smoke_agent_tab_id"
  native automate assert \
    'role=button name="Claude ACP 6".*state=.*selected' \
    'role=group name="Agent conversation"' \
    'role=group name="Recovered Agent history"' \
    'Run the bounded terminal' \
    'Tier 2 terminal completed\.' \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  smoke_composer_id=$(smoke_widget_id 'role=textbox name="Agent prompt".*enabled=true')
  native automate widget-action hyper-term-canvas "$smoke_composer_id" set-text 'Run after application restart'
  smoke_send_id=$(smoke_widget_id 'role=button name="Send prompt".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_send_id"
  native automate assert \
    'Approval required' \
    'Shell command · external effect' \
    'role=button name="Allow once".*enabled=true'
  smoke_allow_id=$(smoke_widget_id 'role=button name="Allow once".*enabled=true')
  native automate widget-click hyper-term-canvas "$smoke_allow_id"
  native automate assert \
    'Tier 2 terminal completed.' \
    'role=button name="Plan complete"' \
    'role=button name="Allowed once"'
  printf '%s\n' \
    "supervisor_before=$smoke_supervisor_before_application_restart" \
    "supervisor_after=$smoke_supervisor_after_application_restart" \
    "renderer_before=$smoke_renderer_before_application_restart" \
    "renderer_after=$smoke_renderer_after_application_restart" \
    'durable_workspace_mode=0600' \
    'durable_agent_binding_mode=0600' \
    'workspace_tabs_restored=true' \
    'active_agent_restored=true' \
    'agent_history_restored=true' \
    'terminal_gateway_token_rotated=true' \
    'agent_provider_process_recreated=true' \
    'agent_history_and_new_turn_verified=true' \
    > .zig-cache/native-sdk-automation/application-restart.txt
  kill -INT "$smoke_pid" 2>/dev/null || true
  wait "$smoke_pid" 2>/dev/null || true
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
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-agent-restored.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-agent-restored.png"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-search.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-search.png"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-goal.png" \
    "$smoke_artifact_dir/screenshot-hyper-term-goal.png"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/renderer-restart.txt" \
    "$smoke_artifact_dir/renderer-restart.txt"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/workspace-before-restart.json" \
    "$smoke_artifact_dir/workspace-before-restart.json"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/workspace-before-application-restart.json" \
    "$smoke_artifact_dir/workspace-before-application-restart.json"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/agent-bindings-before-application-restart.json" \
    "$smoke_artifact_dir/agent-bindings-before-application-restart.json"
  cp \
    "$smoke_root/.zig-cache/native-sdk-automation/application-restart.txt" \
    "$smoke_artifact_dir/application-restart.txt"
  cp "$smoke_log" "$smoke_artifact_dir/hyper-term-smoke.log"
fi

echo "Hyper Term macOS desktop smoke passed"
