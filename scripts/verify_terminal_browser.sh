#!/usr/bin/env bash
set -euo pipefail

verify_repo_root=$(cd "$(dirname "$0")/.." && pwd)
verify_hyperd=${1:-"$verify_repo_root/target/debug/hyperd"}
verify_assets=${2:-"$verify_repo_root/dist/terminal"}
verify_artifact_dir=${HYPER_TERM_TERMINAL_BROWSER_ARTIFACT_DIR:-"$verify_repo_root/.zig-cache/terminal-browser"}
verify_token=0123456789abcdef0123456789abcdef
verify_session="hyper-term-terminal-$$"
verify_root=$(mktemp -d "${TMPDIR:-/tmp}/hyper-term-terminal-browser.XXXXXX")
verify_log="$verify_root/hyperd.log"
verify_pid=""

verify_cleanup() {
  verify_status=$?
  trap - EXIT INT TERM
  agent-browser --session "$verify_session" close >/dev/null 2>&1 || true
  if [[ -n "$verify_pid" ]] && kill -0 "$verify_pid" 2>/dev/null; then
    kill -INT "$verify_pid" 2>/dev/null || true
    wait "$verify_pid" 2>/dev/null || true
  fi
  if [[ $verify_status -ne 0 ]]; then
    echo "Terminal browser verification failed; hyperd log follows:" >&2
    tail -n 60 "$verify_log" >&2 || true
  fi
  if [[ -d "$verify_root" && "$verify_root" == "${TMPDIR:-/tmp}"/hyper-term-terminal-browser.* ]]; then
    rm -r -- "$verify_root"
  fi
  exit "$verify_status"
}
trap verify_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for verify_command in agent-browser grep mkdir; do
  if ! command -v "$verify_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $verify_command" >&2
    exit 1
  fi
done
if [[ ! -x "$verify_hyperd" ]]; then
  echo "built hyperd is unavailable: $verify_hyperd" >&2
  exit 1
fi
if [[ ! -f "$verify_assets/index.html" ]]; then
  echo "built Terminal assets are unavailable: $verify_assets" >&2
  exit 1
fi

umask 077
printf '%s' "$verify_token" > "$verify_root/token"
"$verify_hyperd" \
  --state-dir "$verify_root/state" \
  --terminal-assets "$verify_assets" \
  --terminal-http 127.0.0.1:0 \
  --terminal-token-file "$verify_root/token" \
  >"$verify_log" 2>&1 &
verify_pid=$!

verify_origin=""
for _ in {1..100}; do
  verify_origin=$(grep -Eo 'http://127\.0\.0\.1:[0-9]+' "$verify_log" | tail -n 1 || true)
  [[ -n "$verify_origin" ]] && break
  sleep 0.05
done
if [[ -z "$verify_origin" ]]; then
  echo "terminal gateway did not publish its loopback address" >&2
  exit 1
fi

verify_url="$verify_origin/?token=$verify_token&tab=1"
agent-browser --session "$verify_session" open "$verify_url" >/dev/null
agent-browser --session "$verify_session" wait 500 >/dev/null

verify_boot=$(agent-browser --session "$verify_session" eval \
  'document.querySelector(".xterm-helper-textarea") === document.activeElement && document.querySelector("#terminal")?.dataset.renderer === "webgl" && !document.querySelector(".xterm-accessibility-tree") ? "OK" : "FAIL"')
grep -q '"OK"' <<<"$verify_boot"

agent-browser --session "$verify_session" keyboard type "printf '__HYPER_TERM_BROWSER_INPUT__\\n'" >/dev/null
agent-browser --session "$verify_session" press Enter >/dev/null
agent-browser --session "$verify_session" wait 200 >/dev/null

# The terminal uses a canvas renderer. Select the output row relative to the
# real xterm textarea cursor, then prove Command-F receives xterm's selection.
verify_selection_y=$(agent-browser --session "$verify_session" eval \
  'Math.round(document.querySelector(".xterm-helper-textarea").getBoundingClientRect().top - 9)')
agent-browser --session "$verify_session" mouse move 12 "$verify_selection_y" >/dev/null
agent-browser --session "$verify_session" mouse down left >/dev/null
agent-browser --session "$verify_session" mouse move 310 "$verify_selection_y" >/dev/null
agent-browser --session "$verify_session" mouse up left >/dev/null
agent-browser --session "$verify_session" press Meta+f >/dev/null
verify_selection=$(agent-browser --session "$verify_session" eval \
  'document.querySelector("#terminal-search-input")?.value.includes("__HYPER_TERM_BROWSER_INPUT__") ? "OK" : "FAIL"')
grep -q '"OK"' <<<"$verify_selection"

# A window activation must not steal an active search/IME lease.
verify_search_focus=$(agent-browser --session "$verify_session" eval \
  'window.dispatchEvent(new FocusEvent("focus")); document.activeElement?.id === "terminal-search-input" ? "OK" : "FAIL"')
grep -q '"OK"' <<<"$verify_search_focus"
agent-browser --session "$verify_session" press Escape >/dev/null

# Exercise xterm's real composition listener and preedit surface, not a mock.
verify_composition=$(agent-browser --session "$verify_session" eval \
  '(()=>{const t=document.querySelector(".xterm-helper-textarea");t.focus();t.dispatchEvent(new CompositionEvent("compositionstart",{bubbles:true,data:""}));t.value="拼音";t.dispatchEvent(new CompositionEvent("compositionupdate",{bubbles:true,data:"拼音"}));t.dispatchEvent(new InputEvent("input",{bubbles:true,inputType:"insertCompositionText",data:"拼音",isComposing:true}));const view=document.querySelector(".composition-view");return view?.textContent==="拼音"&&getComputedStyle(view).display!=="none"?"OK":"FAIL"})()')
grep -q '"OK"' <<<"$verify_composition"
verify_composition_end=$(agent-browser --session "$verify_session" eval \
  '(()=>{const t=document.querySelector(".xterm-helper-textarea");t.dispatchEvent(new CompositionEvent("compositionend",{bubbles:true,data:"拼音"}));return getComputedStyle(document.querySelector(".composition-view")).display==="none"?"OK":"FAIL"})()')
grep -q '"OK"' <<<"$verify_composition_end"

agent-browser --session "$verify_session" press Control+c >/dev/null
agent-browser --session "$verify_session" keyboard inserttext '中文输入' >/dev/null
agent-browser --session "$verify_session" press Enter >/dev/null
agent-browser --session "$verify_session" wait 200 >/dev/null
mkdir -p "$verify_artifact_dir"
agent-browser --session "$verify_session" screenshot "$verify_artifact_dir/terminal-input.png" >/dev/null

# Select the shell's rendered diagnostic to prove the committed CJK text made
# the complete Browser -> xterm -> WebSocket -> Rust PTY -> zsh round trip.
verify_cjk_y=$(agent-browser --session "$verify_session" eval \
  'Math.round(document.querySelector(".xterm-helper-textarea").getBoundingClientRect().top - 9)')
agent-browser --session "$verify_session" mouse move 12 "$verify_cjk_y" >/dev/null
agent-browser --session "$verify_session" mouse down left >/dev/null
agent-browser --session "$verify_session" mouse move 310 "$verify_cjk_y" >/dev/null
agent-browser --session "$verify_session" mouse up left >/dev/null
agent-browser --session "$verify_session" press Meta+f >/dev/null
verify_cjk=$(agent-browser --session "$verify_session" eval \
  'document.querySelector("#terminal-search-input")?.value.includes("中文输入") ? "OK" : "FAIL"')
grep -q '"OK"' <<<"$verify_cjk"
agent-browser --session "$verify_session" press Escape >/dev/null

# Accessibility stays off on the high-throughput default path. The discoverable
# focus-only control enables xterm's real row list and live region on demand.
agent-browser --session "$verify_session" focus '#terminal-screen-reader-toggle' >/dev/null
agent-browser --session "$verify_session" press Enter >/dev/null
agent-browser --session "$verify_session" wait 100 >/dev/null
verify_accessibility=$(agent-browser --session "$verify_session" eval \
  '(()=>{const button=document.querySelector("#terminal-screen-reader-toggle");const tree=document.querySelector(".xterm-accessibility-tree");return button?.getAttribute("aria-pressed")==="true"&&tree?.getAttribute("role")==="list"&&tree.querySelectorAll("[role=listitem]").length>0&&tree.textContent.includes("中文输入")?"OK":"FAIL"})()')
grep -q '"OK"' <<<"$verify_accessibility"
agent-browser --session "$verify_session" screenshot "$verify_artifact_dir/terminal-screen-reader-toggle.png" >/dev/null

verify_errors=$(agent-browser --session "$verify_session" errors)
if [[ -n "$verify_errors" ]]; then
  echo "$verify_errors" >&2
  exit 1
fi

echo "Terminal browser verified: WebGL xterm, zsh input, selection, search focus, IME, and opt-in accessibility tree"
echo "Terminal browser screenshot: $verify_artifact_dir/terminal-input.png"
echo "Terminal accessibility screenshot: $verify_artifact_dir/terminal-screen-reader-toggle.png"
