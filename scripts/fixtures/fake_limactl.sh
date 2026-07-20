#!/bin/sh
set -eu

if [ "${1:-}" = "--version" ]; then
  echo 'limactl version 2.1.1'
  exit 0
fi

fixture_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
environment_marker="$fixture_root/limactl-environment"
action=''
last=''
for argument in "$@"; do
  last="$argument"
  case "$argument" in
    validate|start|shell|stop|delete) [ -n "$action" ] || action="$argument" ;;
  esac
done

if [ "$action" = start ]; then
  printf '%s\n' "${last%/*}" > "$environment_marker"
fi
if [ "$action" = shell ]; then
  environment=$(cat "$environment_marker")
  printf 'from tier2\n' > "$environment/worktree/generated.txt"
  printf 'tier2-output\n'
fi
