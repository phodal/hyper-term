import {
  BLOCK_SCHEMA_VERSION,
  type BlockDocument,
  type BlockEnvelope,
} from "./protocol.ts";

const taskId = "019f-demo-task";

function block(
  id: string,
  order: number,
  kind: BlockEnvelope["kind"],
  lifecycle: BlockEnvelope["lifecycle"],
  payload: BlockEnvelope["payload"],
  attention: BlockEnvelope["attention"] = "none",
): BlockEnvelope {
  return {
    schema_version: BLOCK_SCHEMA_VERSION,
    block_id: id,
    block_revision: 1,
    document_revision: order,
    parent_block_id: null,
    order_key: order,
    task_id: taskId,
    kind,
    render_slot: attention === "none" ? "timeline" : "attention",
    trust_class: kind === "message" ? "untrusted_content" : "trusted_chrome",
    lifecycle,
    attention,
    payload,
    actions: [],
  };
}

export const sampleDocument: BlockDocument = {
  schema_version: BLOCK_SCHEMA_VERSION,
  task_id: taskId,
  revision: 8,
  semantic_digest: "demo-snapshot",
  blocks: [
    block("task", 1, "task", "running", {
      type: "task",
      title: "Ship the permissioned Deno Workbench slice",
    }),
    block("message", 2, "message", "running", {
      type: "message",
      role: "agent",
      text:
        "I reduced the work into one exact build and verification operation.",
    }),
    block("operation", 3, "operation", "waiting", {
      type: "operation",
      operation_id: "019f-demo-operation",
      kind: "shell",
      summary: "deno task check && deno task test && deno task build",
      risk: "read_only",
      state: "waiting_human",
    }, "waiting_approval"),
    {
      ...block("approval", 4, "approval", "waiting", {
        type: "approval",
        operation_id: "019f-demo-operation",
        operation_revision: 3,
        prompt: "Run this exact verification command?",
        options: ["allow_once", "reject_once", "cancelled"],
        decision: null,
      }, "waiting_approval"),
      actions: [{
        action_id: "allow_once",
        expected_block_revision: 1,
        risk: "read_only",
        required_capabilities: ["shell"],
      }],
    },
    block("terminal", 5, "terminal", "succeeded", {
      type: "terminal",
      terminal_id: "019f-demo-terminal",
      command: "cargo test --workspace",
      stream_sequence: 18,
      byte_count: 4128,
      exit_code: 0,
    }),
    block("review", 6, "review", "succeeded", {
      type: "review",
      summary: "2 files changed · 19 Rust tests passed · no Vite dependency",
    }, "review_ready"),
  ],
};
