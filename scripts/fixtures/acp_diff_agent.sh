#!/bin/zsh
set -eu
unsetopt BG_NICE

if [[ "${1:-}" == "login" && "${2:-}" == "status" ]]; then
  print -r -- 'authenticated fixture'
  exit 0
fi

typeset mcp_started=0

start_mcp() {
  local request="$1"
  local command_part="${request#*\"command\":\"}"
  local mcp_command="${command_part%%\"*}"
  local socket_part="${request#*\"--socket\",\"}"
  local mcp_socket="${socket_part%%\"*}"
  local task_part="${request#*\"--task-id\",\"}"
  local mcp_task="${task_part%%\"*}"

  if [[ -z "$mcp_command" || -z "$mcp_socket" || -z "$mcp_task" ]]; then
    print -u2 -r -- 'ACP fixture did not receive the brokered MCP launch identity'
    return 1
  fi

  coproc "$mcp_command" \
    --agent-mode \
    --socket "$mcp_socket" \
    --task-id "$mcp_task" \
    --enable-deno-lsp \
    --enable-genui
  mcp_started=1

  print -p -r -- '{"jsonrpc":"2.0","id":11,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"hyper-term-desktop-smoke","version":"1"}}}'
  print -p -r -- '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  print -p -r -- '{"jsonrpc":"2.0","id":12,"method":"tools/list","params":{}}'

  local response
  local initialized=0
  local inventory=0
  while (( ! initialized || ! inventory )); do
    IFS= read -r -p response
    if [[ "$response" == *'"id":11'*'"protocolVersion":"2025-11-25"'* ]]; then
      initialized=1
    elif [[ "$response" == *'"id":12'*'hyper_term.genui.compile'*'hyper_term.lsp.query'* ]]; then
      inventory=1
    fi
  done
}

prompt_request_id() {
  local request="$1"
  local id_part="${request#*\"id\":}"
  local request_id="${id_part%%,*}"
  if [[ "$request_id" != <-> ]]; then
    print -u2 -r -- "ACP fixture could not parse prompt request id: $request"
    return 1
  fi
  print -r -- "$request_id"
}

compile_genui() {
  local request_id="$1"
  if (( ! mcp_started )); then
    print -u2 -r -- 'ACP fixture cannot compile GenUI before MCP initialization'
    return 1
  fi

  print -r -- '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"tool_call","toolCallId":"genui-smoke","title":"Compile Agentic UI","kind":"execute","status":"in_progress","rawInput":{"server":"hyper_term","tool":"hyper_term.genui.compile"}}}}'
  print -p -r -- '{"jsonrpc":"2.0","id":19,"method":"tools/call","params":{"name":"hyper_term.genui.compile","arguments":{"entry":"App.tsx","source":"export default function App(){ return <main data-smoke=\"genui\"><h1>Hyper Term Agentic UI</h1><p>Live Deno build accepted.</p></main>; }"}}}'

  local response=''
  local response_received=0
  if IFS= read -r -t 30 -p response; then
    if [[ "$response" == *'"id":19'* ]]; then
      response_received=1
    fi
  fi
  if (( ! response_received )); then
    print -u2 -r -- "brokered GenUI compile response timed out; last frame: $response"
    return 1
  fi
  if [[ "$response" == *'"isError":true'* || "$response" != *'Accepted GenUI revision'* ]]; then
    print -u2 -r -- "brokered GenUI compile failed: $response"
    return 1
  fi

  print -r -- '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"genui-smoke","status":"completed"}}}'
  print -r -- '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The Agentic UI was compiled by the brokered Deno runtime."}}}}'
  print -r -- "{\"jsonrpc\":\"2.0\",\"id\":${request_id},\"result\":{\"stopReason\":\"end_turn\"}}"
}

while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      print -r -- '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":false,"audio":false,"embeddedContext":true},"mcpCapabilities":{"http":false,"sse":false}} ,"authMethods":[],"agentInfo":{"name":"Hyper Term ACP Diff Fixture","version":"1.0.0"}}}'
      ;;
    *'"method":"session/new"'*)
      start_mcp "$line"
      print -r -- '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"desktop-diff-session"}}'
      ;;
    *'"method":"session/prompt"'*'Generate the Agentic UI'*)
      compile_genui "$(prompt_request_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      print -r -- '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Reviewing the requested edit."}}}}'
      print -r -- '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"tool_call","toolCallId":"edit-readme","title":"Edit README.md","kind":"edit","status":"completed","locations":[{"path":"README.md","line":1}],"content":[{"type":"diff","path":"README.md","oldText":"Hyper Term\n","newText":"Hyper Term\nAI Terminal\n"}]}}}'
      print -r -- '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"desktop-diff-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The proposed file change is ready to review."}}}}'
      request_id="$(prompt_request_id "$line")"
      print -r -- "{\"jsonrpc\":\"2.0\",\"id\":${request_id},\"result\":{\"stopReason\":\"end_turn\"}}"
      ;;
  esac
done
