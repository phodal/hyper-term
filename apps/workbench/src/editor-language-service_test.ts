import { assertEquals, assertRejects } from "@std/assert";
import { ArtifactLanguageService } from "./editor-language-service.ts";

const context = {
  artifactId: "55555555-5555-4555-8555-555555555555",
  sourceRevision: 7,
  documentPath: "/App.tsx",
  sessionId: 2,
  token: "abcdef0123456789abcdef0123456789",
};
const draftFiles = {
  "/App.tsx": "const items = []; items.",
  "/theme.ts": "export const accent = '#d7ff72';",
};

Deno.test("artifact language service binds every request to its Rust context", async () => {
  let captured: Request | undefined;
  const service = new ArtifactLanguageService(context, (input, init) => {
    captured = new Request(
      new URL(String(input), "http://127.0.0.1:55321"),
      init,
    );
    return Promise.resolve(Response.json({
      artifact_id: context.artifactId,
      source_revision: 7,
      document_path: "/App.tsx",
      document_version: 2,
      kind: "completion",
      diagnostics: [],
      completions: [{ label: "map", insert_text: "map" }],
    }));
  });
  const completions = await service.completions(
    draftFiles,
    { line: 0, character: 23 },
    new AbortController().signal,
  );
  assertEquals(completions[0].label, "map");
  if (!captured) throw new Error("request was not captured");
  assertEquals(captured.method, "POST");
  const url = new URL(captured.url);
  assertEquals(url.pathname, `/agent/artifact/${context.artifactId}/lsp`);
  assertEquals(url.searchParams.get("session_id"), "2");
  assertEquals(url.searchParams.get("token"), context.token);
  assertEquals(await captured.json(), {
    source_revision: 7,
    document_path: "/App.tsx",
    draft_files: draftFiles,
    kind: "completion",
    position: { line: 0, character: 23 },
  });
});

Deno.test("artifact language service rejects mismatched Rust responses", async () => {
  const service = new ArtifactLanguageService(
    context,
    () =>
      Promise.resolve(Response.json({
        artifact_id: context.artifactId,
        source_revision: 8,
        document_path: "/App.tsx",
        document_version: 1,
        kind: "diagnostics",
        diagnostics: [],
        completions: [],
      })),
  );
  await assertRejects(
    () =>
      service.diagnostics({
        ...draftFiles,
        "/App.tsx": "export default null;",
      }),
    Error,
    "did not match the editor context",
  );
});
