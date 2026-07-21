import { assertEquals } from "@std/assert";
import type * as Esbuild from "esbuild-wasm";
import {
  compileRequest,
  disposeCompiler,
  initializeCompiler,
} from "./compiler-engine.ts";
import type { CompileRequest } from "./compiler-protocol.ts";

function request(
  revision: number,
  files: Record<string, string>,
): CompileRequest {
  return {
    type: "compile",
    request_id: `request-${revision}`,
    source_revision: revision,
    entrypoint: "/App.tsx",
    files,
  };
}

function output(path: string, text: string): Esbuild.OutputFile {
  return {
    path,
    contents: new TextEncoder().encode(text),
    hash: `hash-${path}`,
    text,
  };
}

Deno.test("compiler reuses rebuild context until the virtual file inventory changes", async () => {
  let contexts = 0;
  let rebuilds = 0;
  let disposals = 0;
  const backend = {
    version: "test-esbuild",
    initialize: () => Promise.resolve(),
    context: () => {
      contexts += 1;
      return Promise.resolve({
        rebuild: () => {
          rebuilds += 1;
          return Promise.resolve({
            errors: [],
            warnings: [],
            outputFiles: [
              output("/artifact.js", `bundle-${rebuilds}`),
              output("/artifact.js.map", "{}"),
            ],
          });
        },
        watch: () => Promise.resolve(),
        serve: () => Promise.reject(new Error("not used")),
        cancel: () => Promise.resolve(),
        dispose: () => {
          disposals += 1;
          return Promise.resolve();
        },
      });
    },
  } as unknown as typeof Esbuild;

  await initializeCompiler(backend, {});
  await compileRequest(request(1, { "/App.tsx": "export default 1" }));
  await compileRequest(request(2, { "/App.tsx": "export default 2" }));
  assertEquals({ contexts, rebuilds, disposals }, {
    contexts: 1,
    rebuilds: 2,
    disposals: 0,
  });

  await compileRequest(request(3, {
    "/App.tsx": "export { default } from './Panel.tsx'",
    "/Panel.tsx": "export default 3",
  }));
  assertEquals({ contexts, rebuilds, disposals }, {
    contexts: 2,
    rebuilds: 3,
    disposals: 1,
  });

  await disposeCompiler();
  assertEquals(disposals, 2);
});
