#!/usr/bin/env bash
set -euo pipefail

# Opt-in release confidence gate. This deliberately talks to the user's real,
# authenticated Codex or Claude account, so it is never part of unattended CI.

real_repo_root=$(cd "$(dirname "$0")/.." && pwd)
real_app=${1:-"$real_repo_root/dist/macos/Hyper Term.app"}
real_renderer=${2:-"$real_repo_root/apps/desktop/zig-out/bin/hyper-term"}
real_expected=${HYPER_TERM_REAL_ACP_EXPECTED_TEXT:-HYPER_TERM_REAL_DESKTOP_ACP_OK}
real_artifact_dir=${HYPER_TERM_REAL_ACP_ARTIFACT_DIR:-}
real_genui=${HYPER_TERM_REAL_ACP_GENUI:-0}
real_hostile=${HYPER_TERM_REAL_ACP_HOSTILE:-0}
real_provider=${HYPER_TERM_REAL_ACP_PROVIDER:-codex}

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
if [[ "$real_genui" != 0 && "$real_genui" != 1 ]]; then
  echo "HYPER_TERM_REAL_ACP_GENUI must be 0 or 1" >&2
  exit 1
fi
if [[ "$real_hostile" != 0 && "$real_hostile" != 1 ]]; then
  echo "HYPER_TERM_REAL_ACP_HOSTILE must be 0 or 1" >&2
  exit 1
fi
if [[ "$real_hostile" == 1 && "$real_genui" != 1 ]]; then
  echo "HYPER_TERM_REAL_ACP_HOSTILE requires HYPER_TERM_REAL_ACP_GENUI=1" >&2
  exit 1
fi
case "$real_provider" in
  codex)
    real_provider_label="Codex ACP"
    real_provider_shortcut="hyper-term.new-codex-acp-agent"
    real_provider_flag="--codex"
    real_agent_command="codex"
    real_adapter_package="@agentclientprotocol/codex-acp"
    ;;
  claude)
    real_provider_label="Claude ACP"
    real_provider_shortcut="hyper-term.new-claude-acp-agent"
    real_provider_flag="--claude"
    real_agent_command="claude"
    real_adapter_package="@agentclientprotocol/claude-agent-acp"
    ;;
  copilot)
    real_provider_label="Copilot ACP"
    real_provider_shortcut="hyper-term.new-copilot-acp-agent"
    real_provider_flag="--copilot"
    real_agent_command="copilot"
    real_adapter_package=""
    ;;
  *)
    echo "HYPER_TERM_REAL_ACP_PROVIDER must be codex, claude, or copilot" >&2
    exit 1
    ;;
esac

if [[ $(uname -s) != Darwin ]]; then
  echo "real $real_provider_label desktop smoke requires macOS" >&2
  exit 1
fi

for real_command in "$real_agent_command" grep native python3 sed stat tail; do
  if ! command -v "$real_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $real_command" >&2
    exit 1
  fi
done

real_agent=$(command -v "$real_agent_command")
if [[ "$real_provider" == codex ]]; then
  if ! "$real_agent" login status 2>&1 | grep -q '^Logged in'; then
    echo "Codex is not authenticated; run 'codex login' before this opt-in smoke" >&2
    exit 1
  fi
elif [[ "$real_provider" == claude ]] &&
  ! "$real_agent" auth status 2>&1 | grep -Eq '"loggedIn"[[:space:]]*:[[:space:]]*true'; then
  echo "Claude is not authenticated; run 'claude auth login' before this opt-in smoke" >&2
  exit 1
elif [[ "$real_provider" == copilot ]] &&
  ! "$real_agent" --version 2>&1 | grep -q '^GitHub Copilot CLI '; then
  echo "GitHub Copilot CLI is unavailable or invalid" >&2
  exit 1
fi

real_supervisor="$real_app/Contents/MacOS/hyper-term"
real_runtime="$real_app/Contents/Resources/runtime"
real_inputs=(
  "$real_supervisor"
  "$real_renderer"
  "$real_runtime/deno"
  "$real_repo_root/dist/terminal/index.html"
  "$real_repo_root/dist/workbench/index.html"
)
if [[ -n "$real_adapter_package" ]]; then
  real_adapter="$real_runtime/acp/node_modules/$real_adapter_package/dist/index.js"
  real_inputs+=("$real_adapter")
fi
for real_path in "${real_inputs[@]}"; do
  if [[ ! -e "$real_path" ]]; then
    echo "real $real_provider_label desktop input is unavailable: $real_path" >&2
    exit 1
  fi
done

if [[ -n "$real_artifact_dir" && "$real_artifact_dir" != /* ]]; then
  real_artifact_dir="$PWD/$real_artifact_dir"
fi

real_root=$(mktemp -d "/tmp/hyper-term-real-${real_provider}-acp.XXXXXX")
real_log="$real_root/hyper-term-real-${real_provider}-acp.log"
real_pid=""

real_copy_evidence() {
  if [[ -z "$real_artifact_dir" ]]; then
    return
  fi
  mkdir -p "$real_artifact_dir"
  if [[ -f "$real_root/.zig-cache/native-sdk-automation/snapshot.txt" ]]; then
    cp \
      "$real_root/.zig-cache/native-sdk-automation/snapshot.txt" \
      "$real_artifact_dir/snapshot.txt"
  fi
  if [[ -f "$real_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png" ]]; then
    cp \
      "$real_root/.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png" \
      "$real_artifact_dir/screenshot-hyper-term-real-${real_provider}-acp.png"
  fi
  if [[ -f "$real_log" ]]; then
    cp "$real_log" "$real_artifact_dir/hyper-term-real-${real_provider}-acp.log"
  fi
  if [[ -f "$real_root/state/events.jsonl" ]]; then
    cp "$real_root/state/events.jsonl" "$real_artifact_dir/events.jsonl"
  fi
  if [[ -f "$real_root/artifact-editor-e2e.json" ]]; then
    cp \
      "$real_root/artifact-editor-e2e.json" \
      "$real_artifact_dir/artifact-editor-e2e.json"
  fi
}

real_cleanup() {
  real_status=$?
  trap - EXIT INT TERM
  if [[ -n "$real_pid" ]] && kill -0 "$real_pid" 2>/dev/null; then
    kill -INT "$real_pid" 2>/dev/null || true
    wait "$real_pid" 2>/dev/null || true
  fi
  real_copy_evidence
  if [[ $real_status -ne 0 ]]; then
    echo "real $real_provider_label desktop smoke failed; supervisor log follows:" >&2
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
  export HYPER_TERM_AGENT_DIAGNOSTICS=1
  exec "$real_supervisor" \
    --ui "$real_renderer" \
    --state-dir "$real_root/state" \
    --terminal-assets "$real_repo_root/dist/terminal" \
    --workbench-assets "$real_repo_root/dist/workbench" \
    --shell-cwd "$real_repo_root" \
    "$real_provider_flag" "$real_agent"
) >"$real_log" 2>&1 &
real_pid=$!

(
  cd "$real_root"
  native automate wait
  native automate assert \
    'ready=true' \
    'gpu_nonblank=true' \
    'role=button name="New Agent tab"'
  native automate shortcut "$real_provider_shortcut"
  native automate assert --timeout-ms 30000 \
    "$real_provider_label" \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'

  real_composer_id=$(real_widget_id 'role=textbox name="Agent prompt".*enabled=true')
  if [[ -z "$real_composer_id" ]]; then
    echo "real $real_provider_label composer widget is unavailable" >&2
    exit 1
  fi
  if [[ "$real_hostile" == 1 ]]; then
    real_source='import React, { useEffect } from "react"; import { traceCheckpoint } from "@hyper/runtime"; export default function App(){ useEffect(() => { const host = globalThis as typeof globalThis & { zero?: unknown; webkit?: { messageHandlers?: unknown } }; const denied = host.zero === undefined && !host.webkit?.messageHandlers; traceCheckpoint(denied ? "security.native_denied" : "security.native_exposed", { denied }); }, []); return React.createElement("main", null, "Native bridge isolation probe"); }'
    real_prompt="Use hyper_term.genui.compile exactly once to compile this exact source with entry App.tsx: $real_source Do not run shell commands or modify workspace files. After the tool succeeds, reply exactly $real_expected."
  elif [[ "$real_genui" == 1 ]]; then
    real_prompt="Use hyper_term.genui.compile exactly once to compile this source with entry App.tsx: export default function App(){ return <main data-hyper-term=\"real-mcp\">$real_expected</main>; }. Do not run shell commands or modify workspace files. After the tool succeeds, reply exactly $real_expected."
  else
    real_prompt="Reply with exactly $real_expected. Do not use tools or modify files."
  fi
  native automate widget-action hyper-term-canvas "$real_composer_id" set-text \
    "$real_prompt"
  native automate assert \
    "role=textbox name=\"Agent prompt\".*$real_expected" \
    'role=button name="Send prompt".*enabled=true'

  real_send_id=$(real_widget_id 'role=button name="Send prompt".*enabled=true')
  if [[ -z "$real_send_id" ]]; then
    echo "real $real_provider_label send widget is unavailable" >&2
    exit 1
  fi
  native automate widget-click hyper-term-canvas "$real_send_id"
  native automate assert --timeout-ms 30000 'role=button name="Stop Agent turn"'
  if [[ "$real_genui" == 1 ]]; then
    native automate assert --timeout-ms 120000 \
      'Approval required' \
      'role=button name="Allow once".*enabled=true'
    real_allow_id=$(real_widget_id 'role=button name="Allow once".*enabled=true')
    if [[ -z "$real_allow_id" ]]; then
      echo "real $real_provider_label GenUI approval button is unavailable" >&2
      exit 1
    fi
    native automate widget-click hyper-term-canvas "$real_allow_id"
  fi
  native automate assert --timeout-ms 150000 \
    "$real_expected" \
    'role=textbox name="Agent prompt".*enabled=true' \
    'role=button name="Send prompt".*enabled=true'
  native automate assert --absent \
    'role=button name="Stop Agent turn"' \
    'invalid ACP message:' \
    'error event=' \
    'dispatch_errors=[1-9]'
  if [[ "$real_genui" == 1 ]]; then
    native automate assert 'succeeded' 'Allowed once'
    grep -q '"type":"artifact_accepted"' "$real_root/state/events.jsonl"
    grep -q '"executor":"hyper-term-mcp","succeeded":true' \
      "$real_root/state/events.jsonl"

    native automate assert --timeout-ms 30000 \
      'role=button name="Open ACP artifact editor".*enabled=true'
    real_editor_id=$(real_widget_id \
      'role=button name="Open ACP artifact editor".*enabled=true')
    if [[ -z "$real_editor_id" ]]; then
      echo "real $real_provider_label artifact editor control is unavailable" >&2
      exit 1
    fi
    native automate widget-click hyper-term-canvas "$real_editor_id"
    native automate assert --timeout-ms 30000 \
      'role=group name="ACP artifact editor"' \
      'role=button name="Close ACP artifact editor".*enabled=true' \
      'hyper-term-genui-view.*surface=artifact.*artifact_id=.*token=[0-9a-f]'

    python3 - \
      .zig-cache/native-sdk-automation/snapshot.txt \
      "$real_expected" \
      "$real_hostile" \
      "$real_root/artifact-editor-e2e.json" <<'PY'
import html
import json
import pathlib
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

snapshot_path, expected, hostile, evidence_path = sys.argv[1:]
snapshot = pathlib.Path(snapshot_path).read_text()
match = re.search(r'hyper-term-genui-view.*url="([^"]+)"', snapshot)
if match is None:
    raise SystemExit("artifact Workbench WebView URL is missing")
workbench_url = html.unescape(match.group(1))
parsed = urllib.parse.urlsplit(workbench_url)
query = urllib.parse.parse_qs(parsed.query)
if parsed.hostname != "127.0.0.1" or query.get("surface") != ["artifact"]:
    raise SystemExit("artifact Workbench is not using its token-bound loopback route")
try:
    artifact_id = query["artifact_id"][0]
    session_id = query["session_id"][0]
    token = query["token"][0]
except (KeyError, IndexError):
    raise SystemExit("artifact Workbench route is missing its Rust context")
if not artifact_id or not session_id.isdigit() or not re.fullmatch(r"[0-9a-f]+", token):
    raise SystemExit("artifact Workbench route has an invalid Rust context")

with urllib.request.urlopen(workbench_url, timeout=10) as response:
    workbench_html = response.read().decode()
    if response.status != 200 or '<div id="root"></div>' not in workbench_html:
        raise SystemExit("artifact Workbench did not return its built index")
asset_paths = re.findall(r'(?:src|href)="([^"]+)"', workbench_html)
if not asset_paths:
    raise SystemExit("artifact Workbench index has no built assets")
for asset_path in asset_paths:
    with urllib.request.urlopen(
        urllib.parse.urljoin(workbench_url, asset_path), timeout=10
    ) as response:
        if response.status != 200 or not response.read(1):
            raise SystemExit(f"artifact Workbench asset is unavailable: {asset_path}")

origin = f"{parsed.scheme}://{parsed.netloc}"
auth_query = urllib.parse.urlencode({"token": token, "session_id": session_id})
artifact_path = urllib.parse.quote(artifact_id, safe="")

def read_json(path, timeout=10):
    with urllib.request.urlopen(f"{origin}{path}?{auth_query}", timeout=timeout) as response:
        if response.status != 200:
            raise SystemExit(f"Rust artifact endpoint returned {response.status}: {path}")
        return json.load(response)

source = read_json(f"/agent/artifact/{artifact_path}/source")
files = source.get("files")
source_marker = "security.native_denied" if hostile == "1" else expected
if (
    source.get("artifact_id") != artifact_id
    or not isinstance(source.get("source_revision"), int)
    or source["source_revision"] < 1
    or not isinstance(files, dict)
    or not files
    or source.get("entrypoint") not in files
    or source_marker not in "\n".join(files.values())
):
    raise SystemExit("Rust artifact source does not match the accepted ACP output")

checkpoint = read_json(f"/agent/artifact/{artifact_path}/editor-state")
if (
    checkpoint.get("artifact_id") != artifact_id
    or checkpoint.get("base_source_revision") != source["source_revision"]
    or checkpoint.get("entrypoint") != source["entrypoint"]
    or checkpoint.get("files") != files
    or checkpoint.get("active_path") not in files
    or checkpoint.get("view") not in {"code", "diff", "trace"}
    or not re.fullmatch(r"[0-9a-f]{64}", checkpoint.get("state_digest", ""))
):
    raise SystemExit("Rust artifact editor checkpoint is not bound to accepted source")

lsp_body = json.dumps({
    "source_revision": source["source_revision"],
    "document_path": checkpoint["active_path"],
    "draft_files": checkpoint["files"],
    "kind": "diagnostics",
}).encode()
lsp_request = urllib.request.Request(
    f"{origin}/agent/artifact/{artifact_path}/lsp?{auth_query}",
    data=lsp_body,
    headers={"content-type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(lsp_request, timeout=30) as response:
    lsp = json.load(response)
if (
    lsp.get("artifact_id") != artifact_id
    or lsp.get("source_revision") != source["source_revision"]
    or lsp.get("document_path") != checkpoint["active_path"]
    or lsp.get("kind") != "diagnostics"
    or not isinstance(lsp.get("document_version"), int)
    or lsp["document_version"] < 1
    or not isinstance(lsp.get("diagnostics"), list)
    or not isinstance(lsp.get("completions"), list)
):
    raise SystemExit("Rust-managed Deno LSP response does not match the editor context")

native_preview_denied = None
if hostile == "1":
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        runtime_trace = read_json(f"/agent/artifact/{artifact_path}/runtime-trace")
        names = [event.get("name") for event in runtime_trace.get("events", [])]
        if "security.native_exposed" in names:
            raise SystemExit("isolated Artifact preview received the Native bridge")
        if "security.native_denied" in names:
            native_preview_denied = True
            break
        time.sleep(0.1)
    if native_preview_denied is not True:
        raise SystemExit("isolated Artifact preview did not journal Native bridge denial")

try:
    urllib.request.urlopen(
        f"{origin}/agent/artifact/{artifact_path}/source"
        f"?token=wrong&session_id={session_id}",
        timeout=5,
    )
except urllib.error.HTTPError as error:
    if error.code != 401:
        raise
else:
    raise SystemExit("artifact source endpoint accepted an invalid token")

evidence = {
    "artifact_id": artifact_id,
    "source_revision": source["source_revision"],
    "entrypoint": source["entrypoint"],
    "checkpoint_revision": checkpoint.get("revision"),
    "checkpoint_view": checkpoint["view"],
    "lsp_document_path": lsp["document_path"],
    "lsp_document_version": lsp["document_version"],
    "lsp_diagnostic_count": len(lsp["diagnostics"]),
    "workbench_asset_count": len(asset_paths),
    "native_preview_denied": native_preview_denied,
}
pathlib.Path(evidence_path).write_text(json.dumps(evidence, indent=2) + "\n")
print("Native ACP artifact editor: opened on demand")
print("artifact Workbench: index and built assets returned HTTP 200")
print("Rust source/checkpoint: bound to the accepted ACP artifact")
print("Rust-managed Deno LSP: diagnostics response matched editor context")
if native_preview_denied:
    print("Native bridge: denied inside the isolated Artifact iframe and journaled by Rust")
PY
  fi
  native automate screenshot hyper-term-canvas
  real_screenshot=.zig-cache/native-sdk-automation/screenshot-hyper-term-canvas.png
  if [[ ! -s "$real_screenshot" ]]; then
    echo "real $real_provider_label screenshot is empty" >&2
    exit 1
  fi
  real_screenshot_bytes=$(stat -f '%z' "$real_screenshot")
  if (( real_screenshot_bytes < 100000 )); then
    echo "real $real_provider_label screenshot is unexpectedly small: $real_screenshot_bytes bytes" >&2
    exit 1
  fi
)

real_copy_evidence

if [[ "$real_genui" == 1 ]]; then
  echo "real $real_provider_label GenUI desktop smoke passed: $real_expected"
else
  echo "real $real_provider_label desktop smoke passed: $real_expected"
fi
