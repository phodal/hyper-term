#!/usr/bin/env bash
set -euo pipefail

capsule_repo_root=$(cd "$(dirname "$0")/.." && pwd)
capsule_supervisor=${1:-"$capsule_repo_root/target/debug/hyper-term-desktop"}
capsule_renderer=${2:-"$capsule_repo_root/apps/desktop/zig-out/bin/hyper-term"}
capsule_fixture=${3:-"$capsule_repo_root/crates/hyper-term-daemon/testdata/bug_capsule_v1.json"}
capsule_artifact_dir=${HYPER_TERM_CAPSULE_SMOKE_ARTIFACT_DIR:-}

if [[ "$capsule_supervisor" != /* ]]; then
  capsule_supervisor="$PWD/$capsule_supervisor"
fi
if [[ "$capsule_renderer" != /* ]]; then
  capsule_renderer="$PWD/$capsule_renderer"
fi
if [[ "$capsule_fixture" != /* ]]; then
  capsule_fixture="$PWD/$capsule_fixture"
fi
if [[ -n "$capsule_artifact_dir" && "$capsule_artifact_dir" != /* ]]; then
  capsule_artifact_dir="$PWD/$capsule_artifact_dir"
fi

if [[ $(uname -s) != Darwin ]]; then
  echo "macOS Capsule smoke requires a macOS host" >&2
  exit 1
fi
for capsule_command in native python3 stat; do
  if ! command -v "$capsule_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $capsule_command" >&2
    exit 1
  fi
done
if [[ ! -x "$capsule_supervisor" ]]; then
  echo "desktop supervisor is unavailable: $capsule_supervisor" >&2
  exit 1
fi
if [[ ! -x "$capsule_renderer" ]]; then
  echo "automation-enabled Native renderer is unavailable: $capsule_renderer" >&2
  exit 1
fi
if [[ ! -f "$capsule_fixture" ]]; then
  echo "Bug Capsule fixture is unavailable: $capsule_fixture" >&2
  exit 1
fi
for capsule_asset in \
  "$capsule_repo_root/dist/terminal/index.html" \
  "$capsule_repo_root/dist/workbench/index.html"; do
  if [[ ! -f "$capsule_asset" ]]; then
    echo "built desktop asset is unavailable: $capsule_asset" >&2
    exit 1
  fi
done

capsule_root=$(mktemp -d)
capsule_log="$capsule_root/hyper-term-capsule-smoke.log"
capsule_pid=""

capsule_cleanup() {
  capsule_status=$?
  trap - EXIT INT TERM
  if [[ -n "$capsule_pid" ]] && kill -0 "$capsule_pid" 2>/dev/null; then
    kill -INT "$capsule_pid" 2>/dev/null || true
    wait "$capsule_pid" 2>/dev/null || true
  fi
  if [[ $capsule_status -ne 0 ]]; then
    echo "Hyper Term Capsule smoke failed; supervisor log follows:" >&2
    tail -n 80 "$capsule_log" >&2 || true
  fi
  rm -rf "$capsule_root"
  exit "$capsule_status"
}
trap capsule_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

(
  cd "$capsule_root"
  exec "$capsule_supervisor" \
    --ui "$capsule_renderer" \
    --state-dir "$capsule_root/state" \
    --terminal-assets "$capsule_repo_root/dist/terminal" \
    --workbench-assets "$capsule_repo_root/dist/workbench" \
    --shell-cwd "$capsule_repo_root" \
    --bug-capsule "$capsule_fixture"
) >"$capsule_log" 2>&1 &
capsule_pid=$!

(
  cd "$capsule_root"
  native automate wait
  native automate assert \
    'ready=true' \
    'gpu_nonblank=true' \
    'role=group name="Offline Bug Capsule"' \
    'replay only' \
    'Rust verified' \
    'hyper-term-terminal-view.*url="zero://inline"' \
    'hyper-term-genui-view.*surface=capsule.*token=[0-9a-f]'
  native automate assert --absent \
    'error event=' \
    'dispatch_errors=[1-9]'

  python3 - .zig-cache/native-sdk-automation/snapshot.txt <<'PY'
import json
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

snapshot = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'hyper-term-genui-view.*url="([^"]+)"', snapshot)
if match is None:
    raise SystemExit("dynamic GenUI WebView URL is missing")
workbench_url = match.group(1).replace("&amp;", "&")
with urllib.request.urlopen(workbench_url, timeout=5) as response:
    workbench_html = response.read().decode()
    if response.status != 200 or '<div id="root"></div>' not in workbench_html:
        raise SystemExit("offline Workbench did not return its built index")

asset_paths = re.findall(r'(?:src|href)="([^"]+)"', workbench_html)
if not asset_paths:
    raise SystemExit("offline Workbench index has no built assets")
for asset_path in asset_paths:
    asset_url = urllib.parse.urljoin(workbench_url, asset_path)
    with urllib.request.urlopen(asset_url, timeout=5) as response:
        if response.status != 200 or not response.read(1):
            raise SystemExit(f"offline Workbench asset is unavailable: {asset_path}")

token_match = re.search(r'[?&]token=([0-9a-f]+)', workbench_url)
port_match = re.search(r'127[.]0[.]0[.]1:([0-9]+)', workbench_url)
if token_match is None or port_match is None:
    raise SystemExit("offline Workbench URL is not a token-bound loopback URL")
capsule_url = (
    f"http://127.0.0.1:{port_match.group(1)}/agent/debug-capsule"
    f"?token={token_match.group(1)}"
)
with urllib.request.urlopen(capsule_url, timeout=5) as response:
    capsule = json.load(response)
if capsule.get("schema_version") != 2 or capsule.get("mode") != "replay_only":
    raise SystemExit("offline Capsule was not verified and migrated by Rust")
if len(capsule.get("accepted_source_digest", "")) != 64:
    raise SystemExit("offline Capsule accepted-source identity is missing")
if len(capsule.get("capsule_digest", "")) != 64:
    raise SystemExit("offline Capsule integrity identity is missing")

try:
    urllib.request.urlopen(
        f"http://127.0.0.1:{port_match.group(1)}/agent/debug-capsule?token=wrong",
        timeout=5,
    )
except urllib.error.HTTPError as error:
    if error.code != 401:
        raise
else:
    raise SystemExit("offline Capsule endpoint accepted an invalid token")

print("dynamic GenUI WebView: mounted")
print("offline Workbench: index and built assets returned HTTP 200")
print("Rust Bug Capsule: schema v2, replay only, integrity-bound")
PY

  native automate screenshot hyper-term-canvas
  capsule_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png
  if [[ ! -s "$capsule_screenshot" ]]; then
    echo "Native Capsule screenshot is empty" >&2
    exit 1
  fi
  capsule_screenshot_bytes=$(stat -f '%z' "$capsule_screenshot")
  if (( capsule_screenshot_bytes < 100000 )); then
    echo "Native Capsule screenshot is unexpectedly small: $capsule_screenshot_bytes bytes" >&2
    exit 1
  fi
)

if [[ -n "$capsule_artifact_dir" ]]; then
  mkdir -p "$capsule_artifact_dir"
  cp \
    "$capsule_root/.zig-cache/native-sdk-automation/snapshot.txt" \
    "$capsule_artifact_dir/snapshot.txt"
  cp \
    "$capsule_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png" \
    "$capsule_artifact_dir/screenshot-hyper-term-capsule.png"
  cp "$capsule_log" "$capsule_artifact_dir/hyper-term-capsule-smoke.log"
fi

echo "Hyper Term macOS Capsule smoke passed"
