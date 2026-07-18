import { assertEquals, assertThrows } from "@std/assert";
import { applyBlockPatch } from "./block-store.ts";
import type { BlockDocument, BlockEnvelope } from "./protocol.ts";

function messageBlock(): BlockEnvelope {
  return {
    schema_version: 1,
    block_id: "message-1",
    block_revision: 1,
    document_revision: 1,
    parent_block_id: null,
    order_key: 1,
    task_id: "task-1",
    kind: "message",
    render_slot: "timeline",
    trust_class: "untrusted_content",
    lifecycle: "running",
    attention: "none",
    payload: { type: "message", role: "agent", text: "hello " },
    actions: [],
  };
}

function document(): BlockDocument {
  return {
    schema_version: 1,
    task_id: "task-1",
    revision: 1,
    semantic_digest: "from-rust",
    blocks: [messageBlock()],
  };
}

Deno.test("renderer applies a revisioned append without mutating its snapshot", () => {
  const original = document();
  const updated = applyBlockPatch(original, {
    stream_sequence: 2,
    base_revision: 1,
    target_revision: 2,
    operations: [{
      type: "append_content",
      block_id: "message-1",
      expected_previous_revision: 1,
      block_revision: 2,
      text: "world",
    }],
  });
  assertEquals(original.blocks[0].payload.text, "hello ");
  assertEquals(updated.blocks[0].payload.text, "hello world");
  assertEquals(updated.blocks[0].block_revision, 2);
});

Deno.test("renderer rejects stale patches", () => {
  assertThrows(
    () =>
      applyBlockPatch(document(), {
        stream_sequence: 3,
        base_revision: 2,
        target_revision: 3,
        operations: [],
      }),
    Error,
    "patch base",
  );
});
