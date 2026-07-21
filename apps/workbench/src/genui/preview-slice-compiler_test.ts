import { assertEquals, assertRejects } from "@std/assert";
import type * as Esbuild from "esbuild-wasm";
// @ts-types="esbuild-wasm/browser-types"
import * as esbuild from "esbuild-wasm/browser";
import type { CompileRequest } from "./compiler-protocol.ts";
import {
  compileRequest,
  disposeCompiler,
  initializeCompiler,
} from "./compiler-engine.ts";
import {
  canUsePreviewSliceCompiler,
  compilePreviewSlice,
  type PreviewTransformBackend,
  resetPreviewSliceCompilerForTest,
} from "./preview-slice-compiler.ts";

const wasmBytes = await Deno.readFile(
  new URL(
    "../../../../.zig-cache/test-runtime/esbuild.wasm",
    import.meta.url,
  ),
);
await esbuild.initialize({
  wasmModule: await WebAssembly.compile(wasmBytes),
  worker: false,
});

function request(
  revision: number,
  files: Record<string, string>,
): CompileRequest {
  return {
    type: "compile",
    request_id: `slice-${revision}`,
    source_revision: revision,
    entrypoint: "/App.ts",
    files,
  };
}

Deno.test("preview slice executes static TS, JSON, and CSS module graphs", async () => {
  resetPreviewSliceCompilerForTest();
  const response = await compilePreviewSlice(
    request(1, {
      "/App.ts":
        `import { label } from "./copy.json"; import "./theme.css"; export default () => label;`,
      "/copy.json": `{"label":"slice-ready"}`,
      "/theme.css": `.card { color: lime; }`,
    }),
    esbuild,
  );
  if (response.type !== "compiled") throw new Error(response.type);
  const mounted = runBundle(response.candidate.bundle);
  assertEquals(mounted, "slice-ready");
  assertEquals(response.candidate.css, `.card { color: lime; }`);
  assertEquals(response.candidate.source_map.includes("sections"), true);
});

Deno.test("preview slice preserves safe circular ESM execution", async () => {
  resetPreviewSliceCompilerForTest();
  const response = await compilePreviewSlice(
    request(1, {
      "/App.ts": `import { a } from "./a.ts"; export default () => a();`,
      "/a.ts": `import { b } from "./b.ts"; export const a = () => "a" + b();`,
      "/b.ts":
        `import { a } from "./a.ts"; export const b = () => typeof a === "function" ? "b" : "x";`,
    }),
    esbuild,
  );
  if (response.type !== "compiled") throw new Error(response.type);
  assertEquals(runBundle(response.candidate.bundle), "ab");
});

Deno.test("preview slice matches full builds across deterministic branched graphs", async () => {
  resetPreviewSliceCompilerForTest();
  const fullBackend = {
    version: esbuild.version,
    initialize: () => Promise.resolve(),
    context: esbuild.context,
  } as unknown as typeof Esbuild;
  await initializeCompiler(fullBackend, {});
  try {
    for (let seed = 1; seed <= 8; seed += 1) {
      const files = branchedGraph(seed, 24);
      const full = await compileRequest(request(seed, files));
      const sliced = await compilePreviewSlice(request(seed, files), esbuild);
      if (full.type !== "compiled" || sliced.type !== "compiled") {
        throw new Error("compiler did not return an artifact");
      }
      assertEquals(
        runBundle(sliced.candidate.bundle),
        runBundle(full.candidate.bundle),
      );
    }
  } finally {
    await disposeCompiler();
  }
});

Deno.test("preview slice transforms only changed modules after the cold graph", async () => {
  resetPreviewSliceCompilerForTest();
  let transforms = 0;
  const backend: PreviewTransformBackend = {
    version: esbuild.version,
    transform: (...args: Parameters<typeof esbuild.transform>) => {
      transforms += 1;
      return esbuild.transform(...args);
    },
  };
  const files = {
    "/App.ts":
      `import { value } from "./value.ts"; export default () => value;`,
    "/value.ts": `export const value = 1;`,
    "/unused.ts": `export const unused = true;`,
  };
  await compilePreviewSlice(request(1, files), backend);
  assertEquals(transforms, 3);
  await compilePreviewSlice(
    request(2, {
      ...files,
      "/value.ts": `export const value = 2;`,
    }),
    backend,
  );
  assertEquals(transforms, 4);
});

Deno.test("preview slice rejects missing declarations before preview execution", async () => {
  resetPreviewSliceCompilerForTest();
  await assertRejects(
    () =>
      compilePreviewSlice(
        request(1, {
          "/App.ts": `import value from "./missing.ts"; export default value;`,
        }),
        esbuild,
      ),
    Error,
    "undeclared virtual import: /missing.ts",
  );
});

Deno.test("preview slice sends semantics-sensitive syntax to the full compiler", () => {
  for (
    const source of [
      `export default import("./lazy.ts")`,
      `export default import.meta.url`,
      `export default require("./legacy.cjs")`,
    ]
  ) {
    assertEquals(
      canUsePreviewSliceCompiler(request(1, {
        "/App.ts": source,
      })),
      false,
    );
  }
  assertEquals(
    canUsePreviewSliceCompiler(request(1, {
      "/App.ts": `import "./theme.css"; export default 1`,
      "/theme.css": `@import "./base.css";`,
      "/base.css": `body { color: lime; }`,
    })),
    false,
  );
});

function branchedGraph(seed: number, count: number): Record<string, string> {
  const files: Record<string, string> = {
    "/App.ts":
      `import { value } from "./node-0.ts"; export default () => value;`,
  };
  for (let index = count - 1; index >= 0; index -= 1) {
    const next = index + 1;
    if (next >= count) {
      files[`/node-${index}.ts`] = `export const value = ${seed + index};`;
      continue;
    }
    const branch = Math.min(count - 1, next + ((seed + index) % 3));
    files[`/node-${index}.ts`] = branch === next
      ? `import { value as next } from "./node-${next}.ts"; export const value = next + ${index};`
      : `import { value as left } from "./node-${next}.ts"; import { value as right } from "./node-${branch}.ts"; export const value = left + right + ${index};`;
  }
  return files;
}

function runBundle(bundle: string): unknown {
  let mounted: unknown;
  const runtime = globalThis as typeof globalThis & {
    __HYPER_MOUNT__?: (component: () => unknown) => void;
  };
  const previousMount = runtime.__HYPER_MOUNT__;
  runtime.__HYPER_MOUNT__ = (component: () => unknown) => {
    mounted = component();
  };
  try {
    new Function(bundle)();
    return mounted;
  } finally {
    runtime.__HYPER_MOUNT__ = previousMount;
  }
}
