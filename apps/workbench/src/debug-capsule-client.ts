import type { RuntimeTraceProjection } from "./runtime-trace-client.ts";

export type BugCapsuleInclusion = "included" | "digest_only" | "excluded";

export interface BugCapsuleInventoryEntry {
  category: string;
  inclusion: BugCapsuleInclusion;
  item_count: number;
  byte_count: number;
  reason: string;
}

export interface BugCapsule {
  schema_version: 1;
  mode: "replay_only";
  artifact: {
    artifact_id: string;
    source_revision: number;
    entrypoint: string;
    content_digest: string;
    compiler: { name: string; version: string };
  };
  runtime: RuntimeTraceProjection;
  runtime_truncated: boolean;
  omitted_runtime_events: number;
  inventory: BugCapsuleInventoryEntry[];
  reproduction: string[];
  capsule_digest: string;
  [key: string]: unknown;
}

interface BugCapsuleContext {
  artifactId: string;
  sourceRevision: number;
  sessionId: number;
  token: string;
}

type Fetch = typeof globalThis.fetch;

const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const MAX_CAPSULE_BYTES = 512 * 1024;
const INVENTORY_INCLUSIONS = new Set<BugCapsuleInclusion>([
  "included",
  "digest_only",
  "excluded",
]);

export class BugCapsuleClient {
  constructor(
    private readonly context: BugCapsuleContext,
    private readonly fetcher: Fetch = (input, init) =>
      globalThis.fetch(input, init),
  ) {}

  async prepare(signal?: AbortSignal): Promise<BugCapsule> {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
    });
    const response = await this.fetcher(
      `/agent/artifact/${
        encodeURIComponent(this.context.artifactId)
      }/debug-capsule?${query}`,
      { cache: "no-store", signal },
    );
    if (!response.ok) {
      throw new Error(
        `Rust Bug Capsule endpoint returned ${response.status}.`,
      );
    }
    const encoded = await response.text();
    if (new TextEncoder().encode(encoded).byteLength > MAX_CAPSULE_BYTES) {
      throw new Error("Rust Bug Capsule exceeded its bounded export size.");
    }
    let capsule: BugCapsule;
    try {
      capsule = JSON.parse(encoded) as BugCapsule;
    } catch {
      throw new Error("Rust Bug Capsule is not valid JSON.");
    }
    if (!validCapsule(capsule, this.context)) {
      throw new Error("Rust Bug Capsule violated its replay-only contract.");
    }
    const digest = await digestUnsignedBugCapsule(capsule);
    if (digest !== capsule.capsule_digest) {
      throw new Error(
        "Rust Bug Capsule failed offline integrity verification.",
      );
    }
    return capsule;
  }
}

export async function digestUnsignedBugCapsule(
  capsule: BugCapsule,
): Promise<string> {
  const unsigned = { ...capsule } as Partial<BugCapsule>;
  delete unsigned.capsule_digest;
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(JSON.stringify(unsigned)),
  );
  return [...new Uint8Array(digest)].map((byte) =>
    byte.toString(16).padStart(2, "0")
  ).join("");
}

export function downloadBugCapsule(capsule: BugCapsule): void {
  const blob = new Blob([JSON.stringify(capsule, null, 2) + "\n"], {
    type: "application/json",
  });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download =
    `hyper-term-${capsule.artifact.artifact_id}.bug-capsule.json`;
  anchor.click();
  URL.revokeObjectURL(url);
}

function validCapsule(
  value: unknown,
  context: BugCapsuleContext,
): value is BugCapsule {
  if (!value || typeof value !== "object") return false;
  const capsule = value as Partial<BugCapsule>;
  const artifact = capsule.artifact;
  const runtime = capsule.runtime;
  return capsule.schema_version === 1 && capsule.mode === "replay_only" &&
    Boolean(artifact) && artifact?.artifact_id === context.artifactId &&
    artifact.source_revision === context.sourceRevision &&
    SHA256_PATTERN.test(artifact.content_digest) &&
    Boolean(runtime) && runtime?.artifact_id === context.artifactId &&
    runtime.source_revision === context.sourceRevision &&
    SHA256_PATTERN.test(runtime.projection_digest) &&
    Array.isArray(runtime.events) && runtime.events.length <= 256 &&
    typeof capsule.runtime_truncated === "boolean" &&
    Number.isSafeInteger(capsule.omitted_runtime_events) &&
    Number(capsule.omitted_runtime_events) >= 0 &&
    Array.isArray(capsule.inventory) && capsule.inventory.length <= 32 &&
    capsule.inventory.every(validInventoryEntry) &&
    Array.isArray(capsule.reproduction) && capsule.reproduction.length <= 16 &&
    capsule.reproduction.every((step) => typeof step === "string") &&
    typeof capsule.capsule_digest === "string" &&
    SHA256_PATTERN.test(capsule.capsule_digest);
}

function validInventoryEntry(
  value: unknown,
): value is BugCapsuleInventoryEntry {
  if (!value || typeof value !== "object") return false;
  const entry = value as Partial<BugCapsuleInventoryEntry>;
  return typeof entry.category === "string" && entry.category.length > 0 &&
    typeof entry.inclusion === "string" &&
    INVENTORY_INCLUSIONS.has(entry.inclusion as BugCapsuleInclusion) &&
    Number.isSafeInteger(entry.item_count) && Number(entry.item_count) >= 0 &&
    Number.isSafeInteger(entry.byte_count) && Number(entry.byte_count) >= 0 &&
    typeof entry.reason === "string" && entry.reason.length > 0;
}
