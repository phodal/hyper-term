import { assertEquals, assertRejects } from "@std/assert";
import { ArtifactDraftPublisher } from "./artifact-draft-publisher.ts";

const context = {
  artifactId: "55555555-5555-4555-8555-555555555555",
  sourceRevision: 7,
  entrypoint: "/App.tsx",
  files: {
    "/App.tsx": "export default function App() { return null; }",
    "/theme.ts": "export const accent = '#d7ff72';",
  },
  sessionId: 2,
  token: "abcdef0123456789abcdef0123456789",
};

const operationId = "66666666-6666-4666-8666-666666666666";
const artifactId = "77777777-7777-4777-8777-777777777777";

Deno.test("artifact publisher waits for approval and returns the Rust artifact", async () => {
  const requests: Request[] = [];
  const responses = [
    {
      operation_id: operationId,
      operation_revision: 3,
      status: "waiting_approval",
    },
    {
      operation_id: operationId,
      operation_revision: 5,
      status: "compiling",
    },
    {
      operation_id: operationId,
      operation_revision: 6,
      status: "accepted",
      artifact: {
        artifact_id: artifactId,
        source_revision: 8,
        entrypoint: "/App.tsx",
        content_digest: "a".repeat(64),
      },
    },
  ];
  const publisher = new ArtifactDraftPublisher(
    context,
    (input, init) => {
      requests.push(
        new Request(new URL(String(input), "http://127.0.0.1:55321"), init),
      );
      return Promise.resolve(Response.json(responses.shift()));
    },
    () => Promise.resolve(),
  );
  const statuses: string[] = [];
  const published = await publisher.publish(
    "export default function App() { return <main>Live</main>; }",
    (update) => statuses.push(update.status),
    new AbortController().signal,
  );
  assertEquals(published.artifact_id, artifactId);
  assertEquals(statuses, ["waiting_approval", "compiling", "accepted"]);
  assertEquals(requests.map((request) => request.method), [
    "POST",
    "GET",
    "GET",
  ]);
  const proposed = await requests[0].json();
  assertEquals(proposed, {
    base_source_revision: 7,
    entrypoint: "/App.tsx",
    files: {
      "/App.tsx": "export default function App() { return <main>Live</main>; }",
      "/theme.ts": "export const accent = '#d7ff72';",
    },
  });
  const statusUrl = new URL(requests[1].url);
  assertEquals(statusUrl.searchParams.get("operation_id"), operationId);
  assertEquals(statusUrl.searchParams.get("token"), context.token);
});

Deno.test("artifact publisher rejects an accepted revision outside its context", async () => {
  const publisher = new ArtifactDraftPublisher(
    context,
    () =>
      Promise.resolve(Response.json({
        operation_id: operationId,
        operation_revision: 6,
        status: "accepted",
        artifact: {
          artifact_id: artifactId,
          source_revision: 9,
          entrypoint: "/App.tsx",
          content_digest: "b".repeat(64),
        },
      })),
    () => Promise.resolve(),
  );
  await assertRejects(
    () =>
      publisher.publish(
        "export default null;",
        () => {},
        new AbortController().signal,
      ),
    Error,
    "did not match the editor context",
  );
});
