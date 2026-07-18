#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <Hyper Term.app> <desktop-supervisor> <terminal-assets>" >&2
  exit 2
fi

app_bundle=$1
desktop_supervisor=$2
terminal_assets=$3
macos_directory="$app_bundle/Contents/MacOS"
resources_directory="$app_bundle/Contents/Resources"
native_renderer="$macos_directory/hyper-term"
packaged_renderer="$macos_directory/hyper-term-ui"
packaged_supervisor="$macos_directory/hyper-term"
packaged_terminal="$resources_directory/terminal"

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
if [[ ! -f "$terminal_assets/index.html" ]]; then
  echo "terminal renderer is missing index.html: $terminal_assets" >&2
  exit 1
fi
if [[ ! -f "$terminal_assets/build-manifest.json" ]]; then
  echo "terminal renderer is missing build-manifest.json: $terminal_assets" >&2
  exit 1
fi

mv "$native_renderer" "$packaged_renderer"
install -m 0755 "$desktop_supervisor" "$packaged_supervisor"
mkdir -p "$resources_directory"
rm -rf "$packaged_terminal"
cp -R "$terminal_assets" "$packaged_terminal"

if [[ ! -x "$packaged_supervisor" || ! -x "$packaged_renderer" ]]; then
  echo "assembled app is missing an executable" >&2
  exit 1
fi

echo "$app_bundle"
