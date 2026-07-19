import { assertEquals, assertRejects } from "@std/assert";
import { WorkspaceApplyPublisher } from "./workspace-apply-publisher.ts";

const context = {
  artifactId: "55555555-5555-4555-8555-555555555555",
  sourceRevision: 7,
  sessionId: 2,
  token: "abcdef0123456789abcdef0123456789",
};

const operationId = "66666666-6666-4666-8666-666666666666";

const mappings = [
  { source_path: "/App.tsx", target_path: "src/App.tsx" },
  { source_path: "/theme.ts", target_path: "src/theme.ts" },
];

function update(status: string, themeTarget = "src/theme.ts") {
  const changes = [
    {
      source_path: "/App.tsx",
      target_path: "src/App.tsx",
      base_digest: "a".repeat(64),
      proposed_digest: "b".repeat(64),
      before: "export default null;",
      after: "export default function App() { return <main>Live</main>; }",
    },
    {
      source_path: "/theme.ts",
      target_path: themeTarget,
      base_digest: null,
      proposed_digest: "c".repeat(64),
      before: "",
      after: "export const accent = 'acid';",
    },
  ];
  return {
    operation_id: operationId,
    operation_revision: status === "waiting_approval" ? 3 : 6,
    status,
    artifact_source_revision: 7,
    ...changes[0],
    transaction_digest: "d".repeat(64),
    changes,
  };
}

Deno.test("workspace apply publisher exposes the Rust diff and waits for approval", async () => {
  const requests: Request[] = [];
  const responses = [
    update("waiting_approval"),
    update("applying"),
    update("applied"),
  ];
  const publisher = new WorkspaceApplyPublisher(
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
  const applied = await publisher.apply(
    mappings,
    (next) => statuses.push(next.status),
    new AbortController().signal,
  );

  assertEquals(applied.status, "applied");
  assertEquals(statuses, ["waiting_approval", "applying", "applied"]);
  assertEquals(requests.map((request) => request.method), [
    "POST",
    "GET",
    "GET",
  ]);
  assertEquals(await requests[0].json(), {
    artifact_source_revision: 7,
    mappings,
  });
  assertEquals(
    new URL(requests[1].url).searchParams.get("operation_id"),
    operationId,
  );
});

Deno.test("workspace apply publisher rejects a response outside its exact mapping set", async () => {
  const publisher = new WorkspaceApplyPublisher(
    context,
    () => Promise.resolve(Response.json(update("applied", "other.ts"))),
    () => Promise.resolve(),
  );
  await assertRejects(
    () =>
      publisher.apply(
        mappings,
        () => {},
        new AbortController().signal,
      ),
    Error,
    "did not match the editor context",
  );
});
