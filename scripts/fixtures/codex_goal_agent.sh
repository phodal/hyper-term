#!/bin/sh
set -eu

if [ "${1:-}" = "login" ] && [ "${2:-}" = "status" ]; then
  printf '%s\n' 'authenticated fixture'
  exit 0
fi

goal_status=active
goal_objective='Ship the compact Agent UI'

while IFS= read -r line; do
  request_id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"userAgent":"Hyper Term Codex Goal Fixture"}}\n' "$request_id"
      ;;
    *'"method":"model/list"'*)
      printf '{"id":%s,"result":{"data":[{"model":"gpt-fixture","displayName":"GPT Fixture","description":"Deterministic desktop smoke model","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Medium"}],"defaultReasoningEffort":"medium","isDefault":true}]}}\n' "$request_id"
      ;;
    *'"method":"skills/list"'*)
      printf '{"id":%s,"result":{"data":[]}}\n' "$request_id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"desktop-goal-thread"}}}\n' "$request_id"
      ;;
    *'"method":"thread/goal/set"'*)
      case "$line" in
        *'"objective":"'*)
          goal_objective=$(printf '%s' "$line" | sed -n 's/.*"objective":"\([^"]*\)".*/\1/p')
          ;;
      esac
      case "$line" in
        *'"status":"paused"'*) goal_status=paused ;;
        *'"status":"active"'*) goal_status=active ;;
      esac
      printf '{"id":%s,"result":{"goal":{"threadId":"desktop-goal-thread","objective":"%s","status":"%s","tokenBudget":50000,"tokensUsed":1200,"timeUsedSeconds":90,"createdAt":1,"updatedAt":2}}}\n' \
        "$request_id" "$goal_objective" "$goal_status"
      ;;
    *'"method":"thread/goal/clear"'*)
      printf '{"id":%s,"result":{"cleared":true}}\n' "$request_id"
      ;;
  esac
done
