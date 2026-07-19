import {
  ArtifactHistoryClient,
  type ArtifactHistoryEntry,
} from "./artifact-history-client.ts";
import { assertEquals, assertRejects } from "@std/assert";

const activeId = "55555555-5555-4555-8555-555555555555";
const previousId = "44444444-4444-4444-8444-444444444444";
const context = {
  activeArtifactId: activeId,
  sessionId: 7,
  token: "0123456789abcdef0123456789abcdef",
};

const entries: ArtifactHistoryEntry[] = [
  entry(activeId, 8, 42),
  entry(previousId, 7, 31),
];

Deno.test("artifact history client binds newest-first history and source to Rust", async () => {
  const requests: URL[] = [];
  const client = new ArtifactHistoryClient(context, (input) => {
    const url = new URL(String(input), "http://hyper-term.test");
    requests.push(url);
    if (url.pathname.endsWith("/history")) {
      return Promise.resolve(Response.json({
        active_artifact_id: activeId,
        entries,
      }));
    }
    return Promise.resolve(Response.json({
      artifact_id: previousId,
      source_revision: 7,
      entrypoint: "/App.tsx",
      files: {
        "/App.tsx": "import { title } from './title.ts';",
        "/title.ts": "export const title = 'previous';",
      },
    }));
  });

  const history = await client.list();
  const source = await client.source(history[1]);

  assertEquals(history, entries);
  assertEquals(source.files["/title.ts"], "export const title = 'previous';");
  assertEquals(requests[0].searchParams.get("session_id"), "7");
  assertEquals(requests[0].searchParams.get("token"), context.token);
  assertEquals(
    requests[1].pathname,
    `/agent/artifact/${activeId}/history/${previousId}/source`,
  );
});

Deno.test("artifact history client rejects stale or malformed Rust projections", async () => {
  const stale = new ArtifactHistoryClient(
    context,
    () =>
      Promise.resolve(Response.json({
        active_artifact_id: previousId,
        entries,
      })),
  );
  await assertRejects(() => stale.list(), Error, "timeline contract");

  const malformedSource = new ArtifactHistoryClient(context, (input) => {
    const url = new URL(String(input), "http://hyper-term.test");
    return Promise.resolve(Response.json(
      url.pathname.endsWith("/history")
        ? { active_artifact_id: activeId, entries }
        : {
          artifact_id: previousId,
          source_revision: 6,
          entrypoint: "/App.tsx",
          files: { "/App.tsx": "stale" },
        },
    ));
  });
  const history = await malformedSource.list();
  await assertRejects(
    () => malformedSource.source(history[1]),
    Error,
    "did not match",
  );
});

function entry(
  artifactId: string,
  sourceRevision: number,
  sequence: number,
): ArtifactHistoryEntry {
  return {
    event_sequence: sequence,
    recorded_at_ms: 1_753_000_000_000 + sequence,
    operation_id: "33333333-3333-4333-8333-333333333333",
    artifact: {
      artifact_id: artifactId,
      source_revision: sourceRevision,
      entrypoint: "/App.tsx",
      content_digest: "a".repeat(64),
      compiler: { name: "esbuild-wasm", version: "0.28.1" },
    },
  };
}
