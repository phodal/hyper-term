#!/bin/sh
set -eu

if [ "${1:-}" = "login" ] && [ "${2:-}" = "status" ]; then
  printf '%s\n' 'authenticated fixture'
  exit 0
fi

while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":false,"audio":false,"embeddedContext":true},"mcpCapabilities":{"http":false,"sse":false}},"authMethods":[],"agentInfo":{"name":"Hyper Term ACP Diff Fixture","version":"1.0.0"}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"desktop-diff-session"}}'
      ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Reviewing the requested edit."}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"tool_call","toolCallId":"edit-readme","title":"Edit README.md","kind":"edit","status":"completed","locations":[{"path":"README.md","line":1}],"content":[{"type":"diff","path":"README.md","oldText":"Hyper Term\n","newText":"Hyper Term\nAI Terminal\n"}]}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The proposed file change is ready to review."}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
      ;;
  esac
done
