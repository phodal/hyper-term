import {
  BLOCK_SCHEMA_VERSION,
  type BlockDocument,
  type BlockEnvelope,
  type BlockPatch,
} from "./protocol.ts";

export function applyBlockPatch(
  document: BlockDocument,
  patch: BlockPatch,
): BlockDocument {
  if (document.schema_version !== BLOCK_SCHEMA_VERSION) {
    throw new Error(`unsupported BlockDocument ${document.schema_version}`);
  }
  if (document.revision !== patch.base_revision) {
    throw new Error(
      `patch base ${patch.base_revision} does not match ${document.revision}`,
    );
  }
  const blocks = new Map(
    document.blocks.map((block) => [block.block_id, structuredClone(block)]),
  );
  for (const operation of patch.operations) {
    switch (operation.type) {
      case "upsert": {
        const existing = blocks.get(operation.block.block_id);
        if (existing && existing.kind !== operation.block.kind) {
          throw new Error(`block ${operation.block.block_id} changed kind`);
        }
        blocks.set(operation.block.block_id, structuredClone(operation.block));
        break;
      }
      case "append_content": {
        const block = requireBlock(blocks, operation.block_id);
        if (block.block_revision !== operation.expected_previous_revision) {
          throw new Error(`block ${operation.block_id} revision mismatch`);
        }
        if (block.payload.type !== "message") {
          throw new Error(`block ${operation.block_id} is not a message`);
        }
        block.payload.text = String(block.payload.text ?? "") + operation.text;
        block.block_revision = operation.block_revision;
        block.document_revision = patch.target_revision;
        break;
      }
      case "remove":
        blocks.delete(operation.block_id);
        break;
    }
  }
  return {
    ...document,
    revision: patch.target_revision,
    // Rust supplies the canonical semantic digest with a snapshot. A renderer
    // patch deliberately invalidates it until that digest is received.
    semantic_digest: "pending_host_digest",
    blocks: [...blocks.values()].sort((left, right) =>
      left.order_key - right.order_key ||
      left.block_id.localeCompare(right.block_id)
    ),
  };
}

function requireBlock(
  blocks: Map<string, BlockEnvelope>,
  blockId: string,
): BlockEnvelope {
  const block = blocks.get(blockId);
  if (!block) {
    throw new Error(`block ${blockId} does not exist`);
  }
  return block;
}
