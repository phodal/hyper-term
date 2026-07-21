#!/usr/bin/env bash
set -euo pipefail

verify_repo_root=$(cd "$(dirname "$0")/.." && pwd)
verify_assets=${1:-"$verify_repo_root/dist/workbench"}
verify_artifact_dir=${HYPER_TERM_WORKBENCH_BROWSER_ARTIFACT_DIR:-"$verify_repo_root/.zig-cache/workbench-browser"}
verify_session="hyper-term-workbench-$$"
verify_root=$(mktemp -d "${TMPDIR:-/tmp}/hyper-term-workbench-browser.XXXXXX")
verify_log="$verify_root/server.log"
verify_pid=""
verify_warm_p95_budget_ms=${HYPER_TERM_GENUI_WARM_P95_BUDGET_MS:-100}

if ! [[ "$verify_warm_p95_budget_ms" =~ ^[0-9]+$ ]] ||
  ((verify_warm_p95_budget_ms < 1 || verify_warm_p95_budget_ms > 1000)); then
  echo "HYPER_TERM_GENUI_WARM_P95_BUDGET_MS must be an integer from 1 to 1000" >&2
  exit 1
fi

verify_cleanup() {
  verify_status=$?
  trap - EXIT INT TERM
  if [[ $verify_status -ne 0 ]]; then
    mkdir -p "$verify_artifact_dir"
    agent-browser --session "$verify_session" \
      screenshot "$verify_artifact_dir/workbench-failure.png" >/dev/null 2>&1 || true
    agent-browser --session "$verify_session" errors >&2 || true
    echo "Workbench browser verification failed; static-server log follows:" >&2
    tail -n 40 "$verify_log" >&2 || true
  fi
  agent-browser --session "$verify_session" close >/dev/null 2>&1 || true
  if [[ -n "$verify_pid" ]] && kill -0 "$verify_pid" 2>/dev/null; then
    kill -TERM "$verify_pid" 2>/dev/null || true
    wait "$verify_pid" 2>/dev/null || true
  fi
  if [[ -d "$verify_root" && "$verify_root" == "${TMPDIR:-/tmp}"/hyper-term-workbench-browser.* ]]; then
    rm -r -- "$verify_root"
  fi
  exit "$verify_status"
}
trap verify_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for verify_command in agent-browser grep mkdir python3; do
  if ! command -v "$verify_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $verify_command" >&2
    exit 1
  fi
done
if [[ ! -f "$verify_assets/index.html" ]]; then
  echo "built Workbench assets are unavailable: $verify_assets" >&2
  exit 1
fi
for verify_asset in compiler.worker.js esbuild.wasm genui/preview.html; do
  if [[ ! -f "$verify_assets/$verify_asset" ]]; then
    echo "built Workbench asset is unavailable: $verify_assets/$verify_asset" >&2
    exit 1
  fi
done

mkdir -p "$verify_artifact_dir"
python3 -u -m http.server 0 \
  --bind 127.0.0.1 \
  --directory "$verify_assets" \
  >"$verify_log" 2>&1 &
verify_pid=$!

verify_origin=""
for _ in {1..100}; do
  verify_origin=$(grep -Eo 'http://127\.0\.0\.1:[0-9]+' "$verify_log" | tail -n 1 || true)
  [[ -n "$verify_origin" ]] && break
  sleep 0.05
done
if [[ -z "$verify_origin" ]]; then
  echo "Workbench static server did not publish its loopback address" >&2
  exit 1
fi

agent-browser --session "$verify_session" open "$verify_origin/" >/dev/null
agent-browser --session "$verify_session" wait --load networkidle >/dev/null
agent-browser --session "$verify_session" set viewport 1800 1100 >/dev/null

verify_boot=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const started=performance.now();const poll=setInterval(()=>{const status=document.querySelector(".compiler-status")?.textContent||"";const runtime=[...document.querySelectorAll(".preview-badges span")].some((item)=>item.textContent?.trim()==="ready");if(status.includes("Preview ready")&&runtime){clearInterval(poll);resolve("OK");}else if(performance.now()-started>15000){clearInterval(poll);reject(new Error(JSON.stringify({status,runtime})));}},50)})')
grep -q '"OK"' <<<"$verify_boot"

verify_shell=$(agent-browser --session "$verify_session" eval \
  'document.body.innerText.trim().length>0&&!document.querySelector("[data-nextjs-dialog],.vite-error-overlay,#webpack-dev-server-client-overlay")?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_shell"

verify_snapshot=$(agent-browser --session "$verify_session" snapshot -i -c)
grep -q 'tab "Code"' <<<"$verify_snapshot"
grep -q 'tab "Diff"' <<<"$verify_snapshot"
grep -q 'tab "Time Travel"' <<<"$verify_snapshot"
grep -q 'Iframe "Accepted Agentic UI artifact"' <<<"$verify_snapshot"
grep -q 'heading "Verification complete"' <<<"$verify_snapshot"

# Drive CodeMirror through its focus and text-input paths, then wait for the
# real esbuild-wasm Worker and isolated preview handshake to accept the edit.
verify_source='export default function App(){return <main><h1>实时预览 ✓</h1><p>Agentic UI</p></main>}'
agent-browser --session "$verify_session" focus '.cm-content' >/dev/null
verify_focus=$(agent-browser --session "$verify_session" eval \
  'document.activeElement?.classList.contains("cm-content")?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_focus"
agent-browser --session "$verify_session" press Meta+a >/dev/null
verify_selection=$(agent-browser --session "$verify_session" eval \
  '(window.getSelection()?.toString().length||0)>100?"OK":"FAIL"')
grep -q '"OK"' <<<"$verify_selection"
agent-browser --session "$verify_session" keyboard inserttext "$verify_source" >/dev/null

verify_reload=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve,reject)=>{const expected="export default function App(){return <main><h1>实时预览 ✓</h1><p>Agentic UI</p></main>}";const started=performance.now();const poll=setInterval(()=>{const status=document.querySelector(".compiler-status")?.textContent||"";const source=document.querySelector(".cm-content")?.textContent||"";const runtime=[...document.querySelectorAll(".preview-badges span")].some((item)=>item.textContent?.trim()==="ready");if(status.includes("Preview ready")&&source===expected&&runtime){clearInterval(poll);resolve("OK");}else if(performance.now()-started>15000){clearInterval(poll);reject(new Error(JSON.stringify({status,source,runtime})));}},50)})')
grep -q '"OK"' <<<"$verify_reload"
verify_reload_snapshot=$(agent-browser --session "$verify_session" snapshot -c)
grep -q 'heading "实时预览 ✓"' <<<"$verify_reload_snapshot"

# Measure real warm editor-to-iframe latency. Each sample replaces the source
# through CodeMirror and is complete only after the isolated preview reports
# the matching accepted revision ready.
verify_diagnostics=$(agent-browser --session "$verify_session" eval \
  '(()=>{if(typeof window.__hyperTermGenUiDiagnostics!=="function")return "FAIL";window.__hyperTermWarmBaseline=window.__hyperTermGenUiDiagnostics().samples.length;return "OK"})()')
grep -q '"OK"' <<<"$verify_diagnostics"
for verify_iteration in {1..12}; do
  verify_benchmark_source="export default function App(){return <main data-warm-edit=\"$verify_iteration\"><h1>实时预览 ✓</h1><p>Agentic UI · $verify_iteration</p></main>}"
  agent-browser --session "$verify_session" focus '.cm-content' >/dev/null
  agent-browser --session "$verify_session" press Meta+a >/dev/null
  agent-browser --session "$verify_session" keyboard inserttext \
    "$verify_benchmark_source" >/dev/null
  verify_warm_sample=$(agent-browser --session "$verify_session" eval \
    "new Promise((resolve,reject)=>{const target=window.__hyperTermWarmBaseline+$verify_iteration;const started=performance.now();const poll=setInterval(()=>{const diagnostics=window.__hyperTermGenUiDiagnostics?.();const status=document.querySelector('.compiler-status')?.textContent||'';const runtime=[...document.querySelectorAll('.preview-badges span')].some((item)=>item.textContent?.trim()==='ready');if(diagnostics&&diagnostics.samples.length>=target&&status.includes('Preview ready')&&runtime){clearInterval(poll);resolve('OK');}else if(performance.now()-started>15000){clearInterval(poll);reject(new Error(JSON.stringify({target,samples:diagnostics?.samples.length,status,runtime})));}},10)})")
  grep -q '"OK"' <<<"$verify_warm_sample"
done

verify_performance=$(agent-browser --session "$verify_session" eval \
  "(()=>{const samples=window.__hyperTermGenUiDiagnostics?.().samples.slice(-12)||[];const durations=samples.map((sample)=>sample.editToPreviewMs).sort((left,right)=>left-right);const percentile=(quantile)=>durations[Math.max(0,Math.ceil(durations.length*quantile)-1)];const result={ok:samples.length===12&&samples.every((sample)=>sample.warm)&&percentile(.95)<=$verify_warm_p95_budget_ms&&samples.every((sample)=>sample.mainThreadLongTaskCount===0),sampleCount:samples.length,p50EditToPreviewMs:percentile(.5),p95EditToPreviewMs:percentile(.95),maxEditToPreviewMs:durations.at(-1),mainThreadLongTaskCount:samples.reduce((total,sample)=>total+sample.mainThreadLongTaskCount,0),maxMainThreadLongTaskMs:Math.max(0,...samples.map((sample)=>sample.maxMainThreadLongTaskMs)),budgetMs:$verify_warm_p95_budget_ms};return JSON.stringify(result)})()")
echo "Workbench warm GenUI performance: $verify_performance"
grep -q '\\"ok\\":true' <<<"$verify_performance"

# Diff must be the real editable CodeMirror merge view, not a static patch.
agent-browser --session "$verify_session" find role tab click --name Diff --exact >/dev/null
agent-browser --session "$verify_session" wait 100 >/dev/null
verify_diff=$(agent-browser --session "$verify_session" eval \
  '(()=>{const editors=[...document.querySelectorAll(".cm-editor")].map((item)=>item.textContent||"");const selected=document.querySelector(".studio-tabs [role=tab][aria-selected=true]")?.textContent?.trim();return document.querySelectorAll(".cm-mergeView").length===1&&editors.length===2&&editors.some((text)=>text.includes("Agent is working"))&&editors.some((text)=>text.includes("实时预览 ✓"))&&selected==="Diff"?"OK":"FAIL"})()')
grep -q '"OK"' <<<"$verify_diff"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/workbench-live-diff.png" >/dev/null

# The narrow layout intentionally scrolls inside workspace-grid because the
# Native host remains a fixed desktop surface. Prove Studio is reachable and
# the document never introduces horizontal overflow at a phone-width probe.
agent-browser --session "$verify_session" set viewport 480 900 >/dev/null
verify_responsive=$(agent-browser --session "$verify_session" eval \
  'new Promise((resolve)=>{const grid=document.querySelector(".workspace-grid");const studio=document.querySelector(".studio");if(!grid||!studio){resolve("FAIL");return;}const scrollable=grid.scrollHeight>grid.clientHeight&&getComputedStyle(grid).overflowY==="auto";grid.scrollTop=studio.offsetTop;requestAnimationFrame(()=>requestAnimationFrame(()=>{const rect=studio.getBoundingClientRect();const noHorizontalOverflow=document.documentElement.scrollWidth<=innerWidth;resolve(scrollable&&grid.scrollTop>0&&rect.top>=0&&rect.top<160&&rect.bottom>0&&noHorizontalOverflow?"OK":"FAIL");}));})')
grep -q '"OK"' <<<"$verify_responsive"
agent-browser --session "$verify_session" \
  screenshot "$verify_artifact_dir/workbench-narrow-studio.png" >/dev/null

verify_errors=$(agent-browser --session "$verify_session" errors)
if [[ -n "$verify_errors" ]]; then
  echo "$verify_errors" >&2
  exit 1
fi

echo "Workbench browser verified: CodeMirror edit, esbuild-wasm live reload, isolated preview, warm p95 budget, editable Diff, and narrow Studio reachability"
echo "Workbench live Diff screenshot: $verify_artifact_dir/workbench-live-diff.png"
echo "Workbench narrow screenshot: $verify_artifact_dir/workbench-narrow-studio.png"
