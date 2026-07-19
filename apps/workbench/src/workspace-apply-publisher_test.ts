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

const appHunk = {
  id: "1".repeat(64),
  base_start: 1,
  base_lines: 1,
  proposed_start: 1,
  proposed_lines: 1,
  patch: "@@ -1,1 +1,1 @@\n-export default null;\n+export default App;\n",
};

const appFooterHunk = {
  id: "2".repeat(64),
  base_start: 12,
  base_lines: 1,
  proposed_start: 12,
  proposed_lines: 1,
  patch: "@@ -12,1 +12,1 @@\n-old footer\n+new footer\n",
};

const themeHunk = {
  id: "3".repeat(64),
  base_start: 0,
  base_lines: 0,
  proposed_start: 1,
  proposed_lines: 1,
  patch: "@@ -0,0 +1,1 @@\n+export const accent = 'acid';\n",
};

const preview = {
  artifact_source_revision: 7,
  review_digest: "9".repeat(64),
  changes: [
    {
      ...mappings[0],
      base_digest: "a".repeat(64),
      artifact_digest: "b".repeat(64),
      before: "export default null;\nold footer\n",
      artifact_after: "export default App;\nnew footer\n",
      hunks: [appHunk, appFooterHunk],
    },
    {
      ...mappings[1],
      base_digest: null,
      artifact_digest: "c".repeat(64),
      before: "",
      artifact_after: "export const accent = 'acid';\n",
      hunks: [themeHunk],
    },
  ],
};

function update(
  status: string,
  selectedMappings = mappings,
  themeTarget = "src/theme.ts",
) {
  const changes = [
    {
      source_path: "/App.tsx",
      target_path: "src/App.tsx",
      base_digest: "a".repeat(64),
      proposed_digest: "b".repeat(64),
      before: "export default null;\nold footer\n",
      after: "export default App;\nold footer\n",
    },
    {
      source_path: "/theme.ts",
      target_path: themeTarget,
      base_digest: null,
      proposed_digest: "c".repeat(64),
      before: "",
      after: "export const accent = 'acid';\n",
    },
  ].filter((change) =>
    selectedMappings.some((mapping) =>
      mapping.source_path === change.source_path
    )
  );
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

Deno.test("workspace apply publisher previews hunks before creating one exact approval", async () => {
  const requests: Request[] = [];
  const responses = [
    preview,
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
  const reviewed = await publisher.preview(
    mappings,
    new AbortController().signal,
  );
  const selections = [
    { ...mappings[0], hunk_ids: [appHunk.id] },
    { ...mappings[1], hunk_ids: [themeHunk.id] },
  ];
  const statuses: string[] = [];
  const applied = await publisher.apply(
    reviewed,
    selections,
    (next) => statuses.push(next.status),
    new AbortController().signal,
  );

  assertEquals(reviewed, preview);
  assertEquals(applied.status, "applied");
  assertEquals(statuses, ["waiting_approval", "applying", "applied"]);
  assertEquals(requests.map((request) => request.method), [
    "POST",
    "POST",
    "GET",
    "GET",
  ]);
  assertEquals(
    new URL(requests[0].url).pathname.endsWith("/workspace-preview"),
    true,
  );
  assertEquals(await requests[0].json(), {
    artifact_source_revision: 7,
    mappings,
  });
  assertEquals(await requests[1].json(), {
    artifact_source_revision: 7,
    review_digest: preview.review_digest,
    mappings: selections,
  });
  assertEquals(
    new URL(requests[2].url).searchParams.get("operation_id"),
    operationId,
  );
});

Deno.test("workspace apply publisher keeps deselected files in the review binding", async () => {
  const requests: Request[] = [];
  const selectedMappings = [mappings[0]];
  const publisher = new WorkspaceApplyPublisher(
    context,
    (input, init) => {
      requests.push(
        new Request(new URL(String(input), "http://127.0.0.1:55321"), init),
      );
      return Promise.resolve(Response.json(
        requests.length === 1 ? preview : update("applied", selectedMappings),
      ));
    },
    () => Promise.resolve(),
  );
  const reviewed = await publisher.preview(
    mappings,
    new AbortController().signal,
  );
  const selections = [
    { ...mappings[0], hunk_ids: [appHunk.id] },
    { ...mappings[1], hunk_ids: [] },
  ];
  const applied = await publisher.apply(
    reviewed,
    selections,
    () => {},
    new AbortController().signal,
  );

  assertEquals(applied.changes.length, 1);
  assertEquals((await requests[1].json()).mappings, selections);
});

Deno.test("workspace apply publisher rejects a response outside its selected mapping set", async () => {
  const publisher = new WorkspaceApplyPublisher(
    context,
    (() => {
      let requestCount = 0;
      return () => {
        requestCount += 1;
        return Promise.resolve(Response.json(
          requestCount === 1
            ? preview
            : update("applied", mappings, "other.ts"),
        ));
      };
    })(),
    () => Promise.resolve(),
  );
  const reviewed = await publisher.preview(
    mappings,
    new AbortController().signal,
  );
  await assertRejects(
    () =>
      publisher.apply(
        reviewed,
        [
          { ...mappings[0], hunk_ids: [appHunk.id] },
          { ...mappings[1], hunk_ids: [themeHunk.id] },
        ],
        () => {},
        new AbortController().signal,
      ),
    Error,
    "did not match the editor context",
  );
});
