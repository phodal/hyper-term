#!/usr/bin/env bash
set -euo pipefail

verify_repo_root=$(cd "$(dirname "$0")/.." && pwd)
verify_artifact_dir=${HYPER_TERM_ARTIFACT_WORKBENCH_BROWSER_ARTIFACT_DIR:-"$verify_repo_root/.zig-cache/artifact-workbench-browser"}
verify_session="hyper-term-artifact-workbench-$$"
verify_root=$(mktemp -d "${TMPDIR:-/tmp}/hyper-term-artifact-workbench-browser.XXXXXX")
verify_log="$verify_root/fixture.log"
verify_pid=""

verify_cleanup() {
  verify_status=$?
  trap - EXIT INT TERM
  if [[ $verify_status -ne 0 ]]; then
    mkdir -p "$verify_artifact_dir"
    agent-browser --session "$verify_session" \
      screenshot "$verify_artifact_dir/artifact-workbench-failure.png" >/dev/null 2>&1 || true
    agent-browser --session "$verify_session" errors >&2 || true
    echo "Artifact Workbench browser verification failed; fixture log follows:" >&2
    tail -n 80 "$verify_log" >&2 || true
  fi
  agent-browser --session "$verify_session" close >/dev/null 2>&1 || true
  if [[ -n "$verify_pid" ]] && kill -0 "$verify_pid" 2>/dev/null; then
    kill -INT "$verify_pid" 2>/dev/null || true
    wait "$verify_pid" 2>/dev/null || true
  fi
  if [[ -d "$verify_root" && "$verify_root" == "${TMPDIR:-/tmp}"/hyper-term-artifact-workbench-browser.* ]]; then
    rm -r -- "$verify_root"
  fi
  exit "$verify_status"
}
trap verify_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for verify_command in agent-browser cargo grep mkdir shasum; do
  if ! command -v "$verify_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $verify_command" >&2
    exit 1
  fi
done

verify_deno=${HYPER_TERM_DENO_PATH:-}
if [[ -z "$verify_deno" ]]; then
  verify_deno=$(command -v deno || true)
fi
if [[ -z "$verify_deno" || "$verify_deno" != /* || ! -x "$verify_deno" ]]; then
  echo "HYPER_TERM_DENO_PATH must identify an absolute executable Deno path" >&2
  exit 1
fi
verify_deno_sha256=${HYPER_TERM_DENO_SHA256:-$(shasum -a 256 "$verify_deno" | awk '{print $1}')}
verify_runtime_root=${HYPER_TERM_GENUI_RUNTIME_ROOT:-"$verify_repo_root/dist/runtime"}
verify_workbench_assets=${HYPER_TERM_WORKBENCH_ASSETS:-"$verify_repo_root/dist/workbench"}
for verify_asset in \
  "$verify_runtime_root/genui-compiler.js" \
  "$verify_runtime_root/esbuild.wasm" \
  "$verify_runtime_root/genui/preview.html" \
  "$verify_workbench_assets/index.html"; do
  if [[ ! -f "$verify_asset" ]]; then
    echo "required built asset is unavailable: $verify_asset" >&2
    exit 1
  fi
done

mkdir -p "$verify_artifact_dir"
cargo build \
  --locked \
  --package hyper-term-daemon \
  --features test-fixtures \
  --bin artifact-workbench-fixture

HYPER_TERM_FIXTURE_ROOT="$verify_root/state" \
HYPER_TERM_DENO_PATH="$verify_deno" \
HYPER_TERM_DENO_SHA256="$verify_deno_sha256" \
HYPER_TERM_GENUI_RUNTIME_ROOT="$verify_runtime_root" \
HYPER_TERM_WORKBENCH_ASSETS="$verify_workbench_assets" \
  "$verify_repo_root/target/debug/artifact-workbench-fixture" >"$verify_log" 2>&1 &
verify_pid=$!

verify_url=""
for _ in {1..200}; do
  verify_url=$(grep '^HYPER_TERM_ARTIFACT_WORKBENCH_URL=' "$verify_log" | tail -n 1 | cut -d= -f2- || true)
  [[ -n "$verify_url" ]] && break
  if ! kill -0 "$verify_pid" 2>/dev/null; then
    echo "Artifact Workbench fixture exited before publishing its URL" >&2
    exit 1
  fi
  sleep 0.05
done
if [[ -z "$verify_url" ]]; then
  echo "Artifact Workbench fixture did not publish its URL" >&2
  exit 1
fi

agent-browser --session "$verify_session" open "$verify_url" >/dev/null
agent-browser --session "$verify_session" wait --load networkidle >/dev/null
agent-browser --session "$verify_session" set viewport 1400 1000 >/dev/null

verify_ready=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const status=document.querySelector(".language-status")?.textContent||"";const source=document.querySelector(".cm-content")?.textContent||"";if(status.includes("Deno LSP · ready")&&source.includes("value.toUpperCase")){clearInterval(poll);resolve("OK");}else if(performance.now()-started>20000){clearInterval(poll);reject(new Error(JSON.stringify({status,source})));}},50)})')
grep -q '"OK"' <<<"$verify_ready"

verify_shell=$(agent-browser --session "$verify_session" eval \
  'document.body.innerText.trim().length>0&&!document.querySelector("[data-nextjs-dialog],.vite-error-overlay,#webpack-dev-server-client-overlay")?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_shell"
verify_snapshot=$(agent-browser --session "$verify_session" snapshot -i -c)
grep -q 'tab "Code"' <<<"$verify_snapshot"
grep -q 'tab "Diff"' <<<"$verify_snapshot"
grep -q 'tab "Time Travel"' <<<"$verify_snapshot"
grep -q 'Iframe "Accepted Agentic UI artifact"' <<<"$verify_snapshot"

# The IDE tabs must use the standard horizontal roving-focus interaction, not
# only expose role=tab as presentation metadata. Drive the real keyboard path
# and prove selection, focus, and the active panel stay aligned.
agent-browser --session "$verify_session" \
  focus '.studio-tabs [data-view="code"]' >/dev/null
agent-browser --session "$verify_session" press ArrowRight >/dev/null
verify_diff_tab=$(agent-browser --session "$verify_session" eval \
  '(()=>{const view=document.activeElement?.getAttribute("data-view");const selected=document.activeElement?.getAttribute("aria-selected");const panel=document.querySelector(".studio-editor")?.id;const tabStops=[...document.querySelectorAll(".studio-tabs [role=tab]")].filter(tab=>tab.tabIndex===0).map(tab=>tab.getAttribute("data-view"));return view==="diff"&&selected==="true"&&panel==="artifact-diff-panel"&&tabStops.length===1&&tabStops[0]==="diff"?"OK":JSON.stringify({view,selected,panel,tabStops});})()')
grep -q '"OK"' <<<"$verify_diff_tab"
agent-browser --session "$verify_session" press End >/dev/null
verify_trace_tab=$(agent-browser --session "$verify_session" eval \
  '(()=>{const view=document.activeElement?.getAttribute("data-view");const panel=document.querySelector(".studio-editor")?.id;return view==="trace"&&panel==="artifact-trace-panel"?"OK":JSON.stringify({view,panel});})()')
grep -q '"OK"' <<<"$verify_trace_tab"
agent-browser --session "$verify_session" press Home >/dev/null
verify_code_tab=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const editor=document.querySelector(".cm-content");const view=document.activeElement?.getAttribute("data-view");const panel=document.querySelector(".studio-editor")?.id;if(view==="code"&&panel==="artifact-code-panel"&&editor?.getAttribute("aria-label")==="Artifact source /main.ts"){clearInterval(poll);resolve("OK");}else if(performance.now()-started>5000){clearInterval(poll);reject(new Error(JSON.stringify({view,panel,label:editor?.getAttribute("aria-label")})));}},25)})')
grep -q '"OK"' <<<"$verify_code_tab"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/artifact-workbench-keyboard-tabs.png" >/dev/null

# The host-owned quality gate must load the exact Rust-accepted bundle into a
# token-free isolated preview, exercise the fixed three-viewport matrix, and
# persist a revision-bound report. Version 1 intentionally remains
# needs_review when host pixel/theme/state evidence is unavailable.
verify_quality=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const gate=document.querySelector(".visual-quality-gate");const summary=gate?.querySelector("summary")?.textContent||"";const state=gate?.className||"";const error=gate?.querySelector("[role=alert]")?.textContent||"";if((state.includes("needs_review")||state.includes("needs_revision"))&&summary.includes("3 viewports")&&!error){clearInterval(poll);resolve(JSON.stringify({state,summary}));}else if(performance.now()-started>20000){clearInterval(poll);reject(new Error(JSON.stringify({state,summary,error})));}},50)})')
grep -q '3 viewports' <<<"$verify_quality"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/artifact-workbench-visual-quality.png" >/dev/null

# Drive CodeMirror through the same contenteditable path as a user and require
# a diagnostic returned by the Rust-managed Deno LSP session to reach the UI.
verify_diagnostic_source=$'export default function App() {\n  const value: string = 42;\n  return value;\n}\n'
agent-browser --session "$verify_session" focus '.cm-content' >/dev/null
agent-browser --session "$verify_session" press Meta+a >/dev/null
agent-browser --session "$verify_session" keyboard inserttext "$verify_diagnostic_source" >/dev/null
verify_diagnostic=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const status=document.querySelector(".language-status")?.textContent||"";const source=document.querySelector(".cm-content")?.textContent||"";const count=document.querySelectorAll(".cm-lintRange-error").length;if(status.includes("Deno LSP · ready")&&source.includes("const value: string = 42")&&count>0){clearInterval(poll);resolve("OK");}else if(performance.now()-started>20000){clearInterval(poll);reject(new Error(JSON.stringify({status,source,count})));}},50)})')
grep -q '"OK"' <<<"$verify_diagnostic"
agent-browser --session "$verify_session" hover '.cm-lintRange-error' >/dev/null
verify_diagnostic_tooltip=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const text=document.querySelector(".cm-tooltip-lint")?.textContent||"";if(text.includes("not assignable to type")&&text.includes("Deno LSP")){clearInterval(poll);resolve("OK");}else if(performance.now()-started>5000){clearInterval(poll);reject(new Error(text));}},50)})')
grep -q '"OK"' <<<"$verify_diagnostic_tooltip"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/artifact-workbench-deno-diagnostic.png" >/dev/null

# Type the member-access dot instead of injecting completion state. The visible
# CodeMirror completion list must contain entries supplied by Deno LSP.
verify_completion_source=$'const value = "ok";\nvalue'
agent-browser --session "$verify_session" focus '.cm-content' >/dev/null
agent-browser --session "$verify_session" press Meta+a >/dev/null
agent-browser --session "$verify_session" keyboard inserttext "$verify_completion_source" >/dev/null
agent-browser --session "$verify_session" keyboard inserttext '.' >/dev/null
agent-browser --session "$verify_session" wait 300 >/dev/null
agent-browser --session "$verify_session" press Control+Space >/dev/null
verify_completion=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const labels=[...document.querySelectorAll(".cm-completionLabel")].map((item)=>item.textContent||"");const visible=Boolean(document.querySelector(".cm-tooltip-autocomplete"));if(visible&&labels.includes("length")&&labels.includes("toUpperCase")){clearInterval(poll);resolve("OK");}else if(performance.now()-started>10000){clearInterval(poll);reject(new Error(JSON.stringify({visible,labels:labels.slice(0,40)})));}},50)})')
grep -q '"OK"' <<<"$verify_completion"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/artifact-workbench-deno-completion.png" >/dev/null

# Replace the draft with a hostile component that probes the real sandbox.
# The iframe reports only the denial booleans to the test harness; it also
# sends an oversized, channel-matching fake boot message that the trusted
# Workbench must ignore without losing its ready state.
agent-browser --session "$verify_session" eval \
  'globalThis.__hyperTermHostileProbe=null;globalThis.addEventListener("message",event=>{if(event.data?.type==="hyper_term_hostile_probe")globalThis.__hyperTermHostileProbe=event.data.result});"OK"' \
  | grep -q '"OK"'
verify_hostile_source=$'import React, { useEffect } from "react";\n\nexport default function App() {\n  useEffect(() => {\n    void (async () => {\n      const denied = {\n        native: typeof globalThis.zero === "undefined" && !(globalThis).webkit?.messageHandlers,\n        cross_origin: false,\n        popup: false,\n        clipboard: false,\n        network: false,\n      };\n      try { void parent.document.body; } catch { denied.cross_origin = true; }\n      const popup = window.open("about:blank", "_blank");\n      denied.popup = popup === null;\n      popup?.close();\n      try {\n        if (!navigator.clipboard) denied.clipboard = true;\n        else await navigator.clipboard.writeText("blocked");\n      } catch { denied.clipboard = true; }\n      try { await fetch("https://example.invalid/hyper-term-probe"); }\n      catch { denied.network = true; }\n      parent.postMessage({\n        type: "hyper_term_preview_boot",\n        schema_version: 1,\n        channel_token: location.hash.slice(1),\n        padding: "x".repeat(70 * 1024),\n      }, "*");\n      parent.postMessage({ type: "hyper_term_hostile_probe", result: denied }, "*");\n    })();\n  }, []);\n  return React.createElement("main", null, "Hostile artifact probe");\n}\n'
agent-browser --session "$verify_session" focus '.cm-content' >/dev/null
agent-browser --session "$verify_session" press Meta+a >/dev/null
agent-browser --session "$verify_session" keyboard inserttext "$verify_hostile_source" >/dev/null
verify_hostile=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const denied=globalThis.__hyperTermHostileProbe;const runtime=document.querySelector(".preview-badges")?.textContent||"";if(denied&&Object.values(denied).every(value=>value===true)&&runtime.includes("ready")&&!runtime.includes("booting")){clearInterval(poll);resolve("OK");}else if(performance.now()-started>15000){clearInterval(poll);reject(new Error(JSON.stringify({denied,runtime})));}},50)})')
grep -q '"OK"' <<<"$verify_hostile"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/artifact-workbench-hostile-denied.png" >/dev/null

verify_errors=$(agent-browser --session "$verify_session" errors)
if [[ -n "$verify_errors" ]]; then
  echo "$verify_errors" >&2
  exit 1
fi

echo "Artifact Workbench browser verified: authenticated Rust Gateway, real Deno LSP diagnostics, and visible CodeMirror completion"
echo "Artifact Workbench diagnostic screenshot: $verify_artifact_dir/artifact-workbench-deno-diagnostic.png"
echo "Artifact Workbench completion screenshot: $verify_artifact_dir/artifact-workbench-deno-completion.png"
echo "Artifact Workbench keyboard tabs screenshot: $verify_artifact_dir/artifact-workbench-keyboard-tabs.png"
echo "Artifact Workbench visual quality screenshot: $verify_artifact_dir/artifact-workbench-visual-quality.png"
echo "Artifact Workbench hostile preview denied native, cross-origin, popup, clipboard, network, and oversized status injection"
echo "Artifact Workbench hostile screenshot: $verify_artifact_dir/artifact-workbench-hostile-denied.png"
