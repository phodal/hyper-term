import { assertEquals, assertRejects } from "@std/assert";
import {
  type BugCapsule,
  BugCapsuleClient,
  digestUnsignedBugCapsule,
} from "./debug-capsule-client.ts";

const artifactId = "55555555-5555-4555-8555-555555555555";
const context = {
  artifactId,
  sourceRevision: 7,
  sessionId: 3,
  token: "0123456789abcdef0123456789abcdef",
};

async function fixture(): Promise<BugCapsule> {
  const capsule = {
    schema_version: 1,
    mode: "replay_only",
    artifact: {
      artifact_id: artifactId,
      source_revision: 7,
      entrypoint: "/App.tsx",
      content_digest: "a".repeat(64),
      compiler: { name: "esbuild-wasm", version: "0.28.1" },
    },
    accepted_source: [],
    outputs: {},
    editor: {},
    runtime: {
      artifact_id: artifactId,
      source_revision: 7,
      projection_digest: "b".repeat(64),
      events: [],
    },
    runtime_truncated: false,
    omitted_runtime_events: 0,
    environment: {},
    inventory: [{
      category: "terminal_output",
      inclusion: "excluded",
      item_count: 0,
      byte_count: 0,
      reason: "Terminal output is excluded by default",
    }],
    reproduction: ["Verify capsule_digest before replay."],
    capsule_digest: "0".repeat(64),
  } satisfies BugCapsule;
  capsule.capsule_digest = await digestUnsignedBugCapsule(capsule);
  return capsule;
}

Deno.test("Bug Capsule client verifies the exact replay-only Rust export", async () => {
  const requests: Request[] = [];
  const capsule = await fixture();
  const client = new BugCapsuleClient(context, (request, init) => {
    requests.push(
      new Request(new URL(String(request), "http://hyper.test"), init),
    );
    return Promise.resolve(Response.json(capsule));
  });

  const prepared = await client.prepare();

  assertEquals(prepared, capsule);
  assertEquals(requests[0].method, "GET");
  assertEquals(new URL(requests[0].url).searchParams.get("session_id"), "3");
});

Deno.test("Bug Capsule client rejects tampering before UI export", async () => {
  const capsule = await fixture();
  capsule.mode = "replay_only";
  capsule.reproduction = ["tampered after digest"];
  const client = new BugCapsuleClient(
    context,
    () => Promise.resolve(Response.json(capsule)),
  );

  await assertRejects(
    () => client.prepare(),
    Error,
    "failed offline integrity verification",
  );
});
