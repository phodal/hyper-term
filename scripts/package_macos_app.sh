#!/usr/bin/env bash
set -euo pipefail

hyper_repo_root=$(cd "$(dirname "$0")/.." && pwd)
hyper_output=${1:-"$hyper_repo_root/dist/macos/Hyper Term.app"}

if [[ $(uname -s) != Darwin ]]; then
  echo "macOS application packaging requires a macOS host" >&2
  exit 1
fi
if [[ -e "$hyper_output" ]]; then
  echo "output already exists; move or remove it first: $hyper_output" >&2
  exit 1
fi
for hyper_command in cargo native codesign plutil; do
  if ! command -v "$hyper_command" >/dev/null 2>&1; then
    echo "required command is unavailable: $hyper_command" >&2
    exit 1
  fi
done

if [[ -n ${HYPER_TERM_DENO:-} ]]; then
  hyper_deno=$HYPER_TERM_DENO
elif [[ -x "$hyper_repo_root/.tools/deno/2.9.3/deno" ]]; then
  hyper_deno="$hyper_repo_root/.tools/deno/2.9.3/deno"
else
  hyper_deno=$(command -v deno || true)
fi
if [[ -z "$hyper_deno" || ! -x "$hyper_deno" ]]; then
  echo "Deno 2.9.3 is required; set HYPER_TERM_DENO to its executable" >&2
  exit 1
fi
if [[ $($hyper_deno --version | head -n 1) != "deno 2.9.3"* ]]; then
  echo "Hyper Term packaging requires the pinned Deno 2.9.3 runtime" >&2
  exit 1
fi

hyper_staging_root=$(mktemp -d)
trap 'rm -rf "$hyper_staging_root"' EXIT
hyper_staging_app="$hyper_staging_root/Hyper Term.app"

cd "$hyper_repo_root"
"$hyper_deno" task check
"$hyper_deno" task test
"$hyper_deno" task build:terminal
cargo build --locked --release --package hyper-term-daemon --bin hyper-term-desktop

cd "$hyper_repo_root/apps/desktop"
native check --strict
native test
native build --release=fast
native package \
  --target macos \
  --output "$hyper_staging_app" \
  --binary zig-out/bin/hyper-term \
  --assets assets \
  --web-engine system \
  --web-layer include \
  --signing none

mv \
  "$hyper_staging_app/Contents/MacOS/hyper-term" \
  "$hyper_staging_app/Contents/MacOS/hyper-term-ui"
install -m 0755 \
  "$hyper_repo_root/target/release/hyper-term-desktop" \
  "$hyper_staging_app/Contents/MacOS/hyper-term"
mkdir -p "$hyper_staging_app/Contents/Resources/terminal"
cp -R \
  "$hyper_repo_root/dist/terminal/." \
  "$hyper_staging_app/Contents/Resources/terminal/"

codesign --force --sign - "$hyper_staging_app/Contents/MacOS/hyper-term-ui"
codesign --force --sign - "$hyper_staging_app/Contents/MacOS/hyper-term"
codesign --force --deep --sign - "$hyper_staging_app"
codesign --verify --deep --strict "$hyper_staging_app"

hyper_bundle_executable=$(plutil -extract CFBundleExecutable raw -o - \
  "$hyper_staging_app/Contents/Info.plist")
if [[ $hyper_bundle_executable != hyper-term ]]; then
  echo "unexpected CFBundleExecutable: $hyper_bundle_executable" >&2
  exit 1
fi
if [[ ! -x "$hyper_staging_app/Contents/MacOS/hyper-term-ui" ]]; then
  echo "packaged Native renderer is unavailable" >&2
  exit 1
fi
if [[ ! -f "$hyper_staging_app/Contents/Resources/terminal/index.html" ]]; then
  echo "packaged terminal renderer is unavailable" >&2
  exit 1
fi

mkdir -p "$(dirname "$hyper_output")"
mv "$hyper_staging_app" "$hyper_output"
echo "$hyper_output"
