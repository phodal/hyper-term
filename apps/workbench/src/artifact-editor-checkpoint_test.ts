import {
  ArtifactEditorCheckpointClient,
  type ArtifactEditorCheckpointContext,
} from "./artifact-editor-checkpoint.ts";

const context: ArtifactEditorCheckpointContext = {
  artifactId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  sourceRevision: 7,
  entrypoint: "/App.tsx",
  files: {
    "/App.tsx": "export default () => null;\n",
    "/theme.ts": "export const color = 'green';\n",
  },
  sessionId: 9,
  token: "0123456789abcdef0123456789abcdef",
};

function checkpoint(revision = 0) {
  return {
    schema_version: 1,
    artifact_id: context.artifactId,
    base_source_revision: 7,
    revision,
    state_digest: "a".repeat(64),
    entrypoint: "/App.tsx",
    files: { ...context.files },
    active_path: "/App.tsx",
    view: "code",
    selections: {},
  };
}

Deno.test("artifact editor checkpoint loads and saves exact Rust state", async () => {
  const requests: Array<{ url: string; init?: RequestInit }> = [];
  const fetcher: typeof fetch = (input, init) => {
    requests.push({ url: String(input), init });
    const revision = init?.method === "PUT" ? 1 : 0;
    return Promise.resolve(Response.json({
      ...checkpoint(revision),
      ...(revision === 1
        ? {
          files: {
            ...context.files,
            "/theme.ts": "export const color = 'amber';\n",
          },
          active_path: "/theme.ts",
          view: "diff",
          selections: { "/theme.ts": { anchor: 7, head: 12 } },
        }
        : {}),
    }));
  };
  const client = new ArtifactEditorCheckpointClient(context, fetcher);
  const loaded = await client.load(new AbortController().signal);
  assertEquals(loaded.revision, 0);
  const saved = await client.save(0, {
    files: {
      ...context.files,
      "/theme.ts": "export const color = 'amber';\n",
    },
    activePath: "/theme.ts",
    view: "diff",
    selections: { "/theme.ts": { anchor: 7, head: 12 } },
  }, new AbortController().signal);
  assertEquals(saved.revision, 1);
  assertEquals(saved.active_path, "/theme.ts");
  assertEquals(requests.length, 2);
  assertEquals(requests[0].init?.method, "GET");
  assertEquals(requests[1].init?.method, "PUT");
  const body = JSON.parse(String(requests[1].init?.body));
  assertEquals(body.expected_revision, 0);
  assertEquals(body.base_source_revision, 7);
  assertEquals(body.selections["/theme.ts"].head, 12);
});

Deno.test("artifact editor checkpoint rejects stale or cross-artifact responses", async () => {
  const stale = new ArtifactEditorCheckpointClient(
    context,
    () => Promise.resolve(new Response("stale", { status: 409 })),
  );
  await assertRejects(
    () =>
      stale.save(0, {
        files: context.files,
        activePath: "/App.tsx",
        view: "code",
        selections: {},
      }, new AbortController().signal),
    "checkpoint is stale",
  );
  const crossed = new ArtifactEditorCheckpointClient(
    context,
    () =>
      Promise.resolve(Response.json({
        ...checkpoint(),
        artifact_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      })),
  );
  await assertRejects(
    () => crossed.load(new AbortController().signal),
    "did not match the current Artifact",
  );
});

Deno.test("artifact editor checkpoint cannot change fixed paths or selections", async () => {
  const client = new ArtifactEditorCheckpointClient(
    context,
    () => Promise.resolve(Response.json(checkpoint())),
  );
  await assertRejects(
    () =>
      client.save(0, {
        files: { "/App.tsx": context.files["/App.tsx"] },
        activePath: "/App.tsx",
        view: "code",
        selections: {},
      }, new AbortController().signal),
    "changed its fixed file set",
  );
  await assertRejects(
    () =>
      client.save(0, {
        files: context.files,
        activePath: "/App.tsx",
        view: "code",
        selections: { "/App.tsx": { anchor: 0, head: 999 } },
      }, new AbortController().signal),
    "checkpoint state is invalid",
  );
});

function assertEquals(actual: unknown, expected: unknown): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(
      `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}

async function assertRejects(
  action: () => Promise<unknown>,
  includes: string,
): Promise<void> {
  try {
    await action();
  } catch (error) {
    if (error instanceof Error && error.message.includes(includes)) return;
    throw error;
  }
  throw new Error(`expected rejection containing ${includes}`);
}
