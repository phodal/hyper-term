#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 6 ]]; then
  echo "usage: $0 <Hyper Term.app> <desktop-supervisor> <mcp-connector> <terminal-assets> <deno-runtime> <genui-runtime-assets>" >&2
  exit 2
fi

app_bundle=$1
desktop_supervisor=$2
mcp_connector=$3
terminal_assets=$4
deno_runtime=$5
genui_runtime_assets=$6
macos_directory="$app_bundle/Contents/MacOS"
resources_directory="$app_bundle/Contents/Resources"
native_renderer="$macos_directory/hyper-term"
packaged_renderer="$macos_directory/hyper-term-ui"
packaged_supervisor="$macos_directory/hyper-term"
packaged_mcp_connector="$macos_directory/hyper-term-mcp"
packaged_terminal="$resources_directory/terminal"
packaged_runtime="$resources_directory/runtime"

if [[ ! -d "$app_bundle" ]]; then
  echo "Native SDK app bundle is missing: $app_bundle" >&2
  exit 1
fi
if [[ ! -x "$native_renderer" ]]; then
  echo "Native SDK renderer is missing: $native_renderer" >&2
  exit 1
fi
if [[ ! -x "$desktop_supervisor" ]]; then
  echo "Rust desktop supervisor is missing: $desktop_supervisor" >&2
  exit 1
fi
if [[ ! -x "$mcp_connector" ]]; then
  echo "Rust MCP connector is missing: $mcp_connector" >&2
  exit 1
fi
if [[ ! -f "$terminal_assets/index.html" ]]; then
  echo "terminal renderer is missing index.html: $terminal_assets" >&2
  exit 1
fi
if [[ ! -f "$terminal_assets/build-manifest.json" ]]; then
  echo "terminal renderer is missing build-manifest.json: $terminal_assets" >&2
  exit 1
fi
if [[ ! -x "$deno_runtime" ]]; then
  echo "pinned Deno runtime is missing: $deno_runtime" >&2
  exit 1
fi
for runtime_asset in genui-compiler.js esbuild.wasm genui/preview.html build-manifest.json; do
  if [[ ! -f "$genui_runtime_assets/$runtime_asset" ]]; then
    echo "GenUI runtime asset is missing: $genui_runtime_assets/$runtime_asset" >&2
    exit 1
  fi
done

mv "$native_renderer" "$packaged_renderer"
install -m 0755 "$desktop_supervisor" "$packaged_supervisor"
install -m 0755 "$mcp_connector" "$packaged_mcp_connector"
mkdir -p "$resources_directory"
rm -rf "$packaged_terminal"
cp -R "$terminal_assets" "$packaged_terminal"
rm -rf "$packaged_runtime"
mkdir -p "$packaged_runtime"
cp -R "$genui_runtime_assets/." "$packaged_runtime/"
install -m 0755 "$deno_runtime" "$packaged_runtime/deno"

if [[ ! -x "$packaged_supervisor" \
  || ! -x "$packaged_renderer" \
  || ! -x "$packaged_mcp_connector" ]]; then
  echo "assembled app is missing an executable" >&2
  exit 1
fi
if [[ ! -x "$packaged_runtime/deno" \
  || ! -f "$packaged_runtime/genui-compiler.js" \
  || ! -f "$packaged_runtime/esbuild.wasm" \
  || ! -f "$packaged_runtime/genui/preview.html" \
  || ! -f "$packaged_runtime/acp/manifest.json" \
  || ! -f "$packaged_runtime/acp/node_modules/@agentclientprotocol/codex-acp/dist/index.js" \
  || ! -f "$packaged_runtime/acp/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js" ]]; then
  echo "assembled app is missing the brokered GenUI/ACP runtime" >&2
  exit 1
fi

echo "$app_bundle"
