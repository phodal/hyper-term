#!/usr/bin/env bash
set -euo pipefail

verify_repo_root=$(cd "$(dirname "$0")/.." && pwd)
verify_assets=${1:-"$verify_repo_root/dist/web-renderers"}
verify_artifact_dir=${HYPER_TERM_WEB_RENDERER_BROWSER_ARTIFACT_DIR:-"$verify_repo_root/.zig-cache/web-renderer-kit-browser"}
verify_session="hyper-term-web-renderer-kit-$$"
verify_root=$(mktemp -d "${TMPDIR:-/tmp}/hyper-term-web-renderer-kit.XXXXXX")
verify_ready="$verify_root/server.ready"
verify_log="$verify_root/server.log"
verify_pid=""

verify_cleanup() {
  verify_status=$?
  trap - EXIT INT TERM
  if [[ $verify_status -ne 0 ]]; then
    agent-browser --session "$verify_session" screenshot \
      "$verify_artifact_dir/web-renderer-kit-failure.png" >/dev/null 2>&1 || true
    agent-browser --session "$verify_session" errors >&2 || true
    tail -n 40 "$verify_log" >&2 || true
  fi
  agent-browser --session "$verify_session" close >/dev/null 2>&1 || true
  if [[ -n "$verify_pid" ]] && kill -0 "$verify_pid" 2>/dev/null; then
    kill -TERM "$verify_pid" 2>/dev/null || true
    wait "$verify_pid" 2>/dev/null || true
  fi
  if [[ -d "$verify_root" && "$verify_root" == "${TMPDIR:-/tmp}"/hyper-term-web-renderer-kit.* ]]; then
    rm -r -- "$verify_root"
  fi
  exit "$verify_status"
}
trap verify_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for verify_command in agent-browser deno grep mkdir; do
  if ! command -v "$verify_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $verify_command" >&2
    exit 1
  fi
done
for verify_asset in index.html manifest.json workbench/index.html workbench/esbuild.wasm terminal/index.html; do
  if [[ ! -f "$verify_assets/$verify_asset" ]]; then
    echo "Web Renderer Kit asset is unavailable: $verify_assets/$verify_asset" >&2
    exit 1
  fi
done

mkdir -p "$verify_artifact_dir"
deno run --allow-read="$verify_assets" \
  "$verify_repo_root/scripts/verify_web_renderer_package.ts" \
  "$verify_assets"
deno run \
  --allow-net=127.0.0.1 \
  --allow-read="$verify_assets" \
  --allow-write="$verify_ready" \
  "$verify_repo_root/scripts/serve_verification_assets.ts" \
  "$verify_assets" \
  "$verify_ready" \
  >"$verify_log" 2>&1 &
verify_pid=$!

verify_origin=""
for _ in {1..200}; do
  [[ -f "$verify_ready" ]] && verify_origin=$(<"$verify_ready")
  [[ -n "$verify_origin" ]] && break
  if ! kill -0 "$verify_pid" 2>/dev/null; then
    echo "Web Renderer Kit server exited before publishing its address" >&2
    exit 1
  fi
  sleep 0.05
done
if [[ -z "$verify_origin" ]]; then
  echo "Web Renderer Kit server did not publish its address" >&2
  exit 1
fi

agent-browser --session "$verify_session" open "$verify_origin/" >/dev/null
agent-browser --session "$verify_session" wait --load networkidle >/dev/null
agent-browser --session "$verify_session" set viewport 1280 820 >/dev/null
verify_launcher=$(agent-browser --session "$verify_session" eval \
  'document.querySelector("a[href=\"workbench/index.html\"]")&&document.querySelector("a[href=\"terminal/index.html\"]")&&document.body.innerText.includes("WebAssembly")&&document.documentElement.scrollWidth===document.documentElement.clientWidth?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_launcher"
agent-browser --session "$verify_session" screenshot \
  "$verify_artifact_dir/web-renderer-kit.png" >/dev/null

agent-browser --session "$verify_session" set viewport 390 844 >/dev/null
verify_narrow=$(agent-browser --session "$verify_session" eval \
  'getComputedStyle(document.querySelector(".surfaces")).gridTemplateColumns.split(" ").length===1&&document.documentElement.scrollWidth===document.documentElement.clientWidth?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_narrow"
agent-browser --session "$verify_session" set media light >/dev/null
agent-browser --session "$verify_session" wait 100 >/dev/null
verify_light=$(agent-browser --session "$verify_session" eval \
  'getComputedStyle(document.documentElement).getPropertyValue("--background").trim()==="#f7f9f1"?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_light"
agent-browser --session "$verify_session" screenshot \
  "$verify_artifact_dir/web-renderer-kit-narrow-light.png" >/dev/null

agent-browser --session "$verify_session" open "$verify_origin/workbench/index.html" >/dev/null
agent-browser --session "$verify_session" wait --load networkidle >/dev/null
verify_workbench=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const status=document.querySelector(".compiler-status")?.textContent||"";const preview=[...document.querySelectorAll(".preview-badges span")].some((item)=>item.textContent?.trim()==="ready");if(status.includes("Preview ready")&&preview){clearInterval(poll);resolve("OK");}else if(performance.now()-started>15000){clearInterval(poll);reject(new Error(JSON.stringify({status,preview})));}},50)})')
grep -q '"OK"' <<<"$verify_workbench"
verify_genui=$(agent-browser --session "$verify_session" eval \
  'document.querySelector(".cm-content")&&document.querySelector("iframe[title=\"Accepted Agentic UI artifact\"]")?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_genui"

verify_errors=$(agent-browser --session "$verify_session" errors)
if [[ -n "$verify_errors" ]]; then
  echo "$verify_errors" >&2
  exit 1
fi
echo "Web Renderer Kit browser verified: responsive launcher, light theme, SHA-256 inventory, and live WebAssembly Workbench"
