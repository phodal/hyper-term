#!/bin/sh
set -eu

while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*'"terminal":true'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{},"authMethods":[],"agentInfo":{"name":"Hyper Term ACP Terminal Fixture","version":"1.0.0"}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"desktop-terminal-session"}}'
      ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-terminal-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Requesting an isolated Tier 2 terminal."}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-terminal-session","update":{"sessionUpdate":"plan","entries":[{"content":"Run the isolated terminal","priority":"high","status":"in_progress"},{"content":"Review the retained result","priority":"medium","status":"pending"}]}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":"terminal-create-1","method":"terminal/create","params":{"sessionId":"desktop-terminal-session","command":"printf","args":["tier2-output\\n"],"outputByteLimit":4096}}'
      ;;
    *'"id":"terminal-create-1"'*'"terminalId"'*)
      terminal_id=$(printf '%s' "$line" | sed -n 's/.*"terminalId":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":"terminal-output-1","method":"terminal/output","params":{"sessionId":"desktop-terminal-session","terminalId":"%s"}}\n' "$terminal_id"
      ;;
    *'"id":"terminal-output-1"'*)
      printf '{"jsonrpc":"2.0","id":"terminal-wait-1","method":"terminal/wait_for_exit","params":{"sessionId":"desktop-terminal-session","terminalId":"%s"}}\n' "$terminal_id"
      ;;
    *'"id":"terminal-wait-1"'*)
      printf '{"jsonrpc":"2.0","id":"terminal-release-1","method":"terminal/release","params":{"sessionId":"desktop-terminal-session","terminalId":"%s"}}\n' "$terminal_id"
      ;;
    *'"id":"terminal-release-1"'*)
      printf '%s\n' \
        '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-terminal-session","update":{"sessionUpdate":"plan","entries":[{"content":"Run the isolated terminal","priority":"high","status":"completed"},{"content":"Review the retained result","priority":"medium","status":"completed"}]}}}' \
        '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-terminal-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Tier 2 terminal completed."}}}}' \
        '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
      ;;
  esac
done
