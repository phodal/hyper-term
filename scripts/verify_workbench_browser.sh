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

# Exercise the exact production Worker with dependency graphs at every scale
# required by ADR 0005. This stays outside the product API and logs timing-only
# evidence while checking source maps and real latest-revision cancellation.
verify_scale_program=""
IFS= read -r -d '' verify_scale_program <<'JAVASCRIPT' || true
(() => {
const benchmark = { status: "running" };
window.__hyperTermGenUiScaleBenchmark = benchmark;
void new Promise((resolve, reject) => {
  const worker = new Worker(new URL("compiler.worker.js", document.baseURI), {
    type: "module",
    name: "hyper-term-genui-scale-verifier",
  });
  const waiting = new Map();
  const buffered = new Map();
  const longTasks = [];
  let revision = 10000;
  let observer;
  if (typeof PerformanceObserver !== "undefined" && PerformanceObserver.supportedEntryTypes.includes("longtask")) {
    observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) longTasks.push(entry.duration);
    });
    observer.observe({ entryTypes: ["longtask"] });
  }
  const finish = (callback, value) => {
    observer?.disconnect();
    worker.terminate();
    callback(value);
  };
  const timeout = setTimeout(() => finish(reject, new Error("GenUI scale benchmark timed out")), 240000);
  const receive = (response) => {
    const waiter = waiting.get(response.request_id);
    if (waiter) {
      waiting.delete(response.request_id);
      waiter(response);
    } else {
      buffered.set(response.request_id, response);
    }
  };
  worker.onmessage = (event) => receive(event.data);
  worker.onerror = (event) => {
    clearTimeout(timeout);
    finish(reject, new Error(event.message || "GenUI scale Worker failed"));
  };
  const waitFor = (requestId) => new Promise((accept) => {
    const response = buffered.get(requestId);
    if (response) {
      buffered.delete(requestId);
      accept(response);
    } else {
      waiting.set(requestId, accept);
    }
  });
  const modulePath = (index) => `/module-${String(index).padStart(4, "0")}.ts`;
  const filesAtScale = (count, marker) => {
    const files = {};
    files["/App.tsx"] = `import { value } from "./module-0001.ts"; export default function App(){return <main>${marker}:{value}</main>}`;
    for (let index = 1; index < count; index += 1) {
      files[modulePath(index)] = index === count - 1
        ? `export const value=${marker};`
        : `import { value as next } from "./module-${String(index + 1).padStart(4, "0")}.ts"; export const value=next+${index};`;
    }
    return files;
  };
  const post = (files, label) => {
    const requestId = `${label}-${crypto.randomUUID()}`;
    worker.postMessage({
      type: "compile",
      request_id: requestId,
      source_revision: ++revision,
      entrypoint: "/App.tsx",
      files,
    });
    return { requestId, response: waitFor(requestId) };
  };
  const compile = async (files, count, label) => {
    const started = performance.now();
    const sent = post(files, label);
    const response = await sent.response;
    const durationMs = Math.round((performance.now() - started) * 100) / 100;
    if (response.type !== "compiled") {
      throw new Error(`${label} returned ${response.type}`);
    }
    const candidate = response.candidate;
    const sourceMap = JSON.parse(candidate.source_map);
    const mappedSources = Array.isArray(sourceMap.sections)
      ? sourceMap.sections.flatMap((section) => section.map?.sources || [])
      : sourceMap.sources || [];
    const mappedModules = mappedSources.filter((source) =>
      source.includes("/App.tsx") || source.includes("/module-")
    ).length;
    if (mappedModules < count || !/^[0-9a-f]{64}$/.test(candidate.content_digest)) {
      throw new Error(`${label} emitted an incomplete source map or digest`);
    }
    return {
      durationMs,
      bundleBytes: new TextEncoder().encode(candidate.bundle).byteLength,
      sourceMapBytes: new TextEncoder().encode(candidate.source_map).byteLength,
      mappedModules,
    };
  };
  const percentile = (values, quantile) => {
    const sorted = [...values].sort((left, right) => left - right);
    return sorted[Math.max(0, Math.ceil(sorted.length * quantile) - 1)];
  };
  (async () => {
    const scales = [];
    for (const count of [100, 500, 1000]) {
      const files = filesAtScale(count, 0);
      const initial = await compile(files, count, `scale-${count}-initial`);
      const rebuilds = [];
      for (let iteration = 1; iteration <= 5; iteration += 1) {
        files[modulePath(count - 1)] = `export const value=${iteration};`;
        rebuilds.push(await compile(files, count, `scale-${count}-rebuild-${iteration}`));
      }
      const rebuildTimes = rebuilds.map((sample) => sample.durationMs);
      scales.push({
        modules: count,
        initialMs: initial.durationMs,
        rebuildP50Ms: percentile(rebuildTimes, 0.5),
        rebuildP95Ms: percentile(rebuildTimes, 0.95),
        bundleBytes: rebuilds.at(-1).bundleBytes,
        sourceMapBytes: rebuilds.at(-1).sourceMapBytes,
        mappedModules: rebuilds.at(-1).mappedModules,
      });
    }

    const burstFiles = filesAtScale(1000, 100);
    const burst = [];
    for (let iteration = 0; iteration < 12; iteration += 1) {
      burstFiles[modulePath(999)] = `export const value=${100 + iteration};`;
      burst.push(post({ ...burstFiles }, `scale-cancel-${iteration}`));
    }
    const burstResponses = await Promise.all(burst.map((entry) => entry.response));
    const superseded = burstResponses.filter((response) => response.type === "compile_superseded").length;
    const last = burstResponses.at(-1);
    if (last.type !== "compiled" || superseded === 0) {
      throw new Error(`latest-revision cancellation failed: last=${last.type} superseded=${superseded}`);
    }

    clearTimeout(timeout);
    finish(resolve, JSON.stringify({
      ok: true,
      scales,
      cancellation: { requests: burst.length, superseded, last: last.type },
      mainThreadLongTaskCount: longTasks.length,
      maxMainThreadLongTaskMs: Math.round(Math.max(0, ...longTasks) * 100) / 100,
    }));
  })().catch((error) => {
    clearTimeout(timeout);
    finish(reject, error);
  });
}).then((value) => {
  benchmark.status = "complete";
  benchmark.result = JSON.parse(value);
}).catch((error) => {
  benchmark.status = "failed";
  benchmark.error = String(error?.message || error).slice(0, 4096);
});
return "STARTED";
})()
JAVASCRIPT
verify_scale_start=$(agent-browser --session "$verify_session" eval "$verify_scale_program")
grep -q '"STARTED"' <<<"$verify_scale_start"
verify_scale=""
for _ in {1..120}; do
  verify_scale=$(agent-browser --session "$verify_session" eval \
    'JSON.stringify(window.__hyperTermGenUiScaleBenchmark||{status:"missing"})')
  if grep -q '\\"status\\":\\"complete\\"' <<<"$verify_scale" ||
    grep -q '\\"status\\":\\"failed\\"' <<<"$verify_scale"; then
    break
  fi
  sleep 2
done
echo "Workbench GenUI scale benchmark: $verify_scale"
grep -q '\\"status\\":\\"complete\\"' <<<"$verify_scale"
grep -q '\\"ok\\":true' <<<"$verify_scale"

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

echo "Workbench browser verified: CodeMirror edit, esbuild-wasm live reload, 100/500/1000-module rebuilds, cancellation, isolated preview, warm p95 budget, editable Diff, and narrow Studio reachability"
echo "Workbench live Diff screenshot: $verify_artifact_dir/workbench-live-diff.png"
echo "Workbench narrow screenshot: $verify_artifact_dir/workbench-narrow-studio.png"
