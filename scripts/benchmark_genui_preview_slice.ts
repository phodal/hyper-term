// @ts-types="esbuild-wasm/browser-types"
import * as esbuild from "esbuild-wasm/browser";
import type { CompileRequest } from "../apps/workbench/src/genui/compiler-protocol.ts";
import {
  compilePreviewSlice,
  resetPreviewSliceCompilerForTest,
} from "../apps/workbench/src/genui/preview-slice-compiler.ts";

const wasmPath = new URL(
  "../esbuild.wasm",
  import.meta.resolve("esbuild-wasm/browser"),
);
await esbuild.initialize({
  wasmModule: await WebAssembly.compile(await Deno.readFile(wasmPath)),
  worker: false,
});

const results = [];
for (const moduleCount of [100, 500, 1_000]) {
  resetPreviewSliceCompilerForTest();
  const files = graph(moduleCount, 0);
  const cold = await measured(() =>
    compilePreviewSlice(request(1, files), esbuild)
  );
  const warm: number[] = [];
  let last = cold.value;
  for (let revision = 2; revision <= 13; revision += 1) {
    const changed = {
      ...files,
      [`/Module${pad(moduleCount - 2)}.ts`]:
        `export const value = ${revision};`,
    };
    const sample = await measured(() =>
      compilePreviewSlice(request(revision, changed), esbuild)
    );
    warm.push(sample.elapsedMs);
    last = sample.value;
  }
  if (last.type !== "compiled") throw new Error(last.type);
  results.push({
    modules: moduleCount,
    cold_ms: round(cold.elapsedMs),
    warm_p50_ms: round(percentile(warm, 0.5)),
    warm_p95_ms: round(percentile(warm, 0.95)),
    warm_max_ms: round(Math.max(...warm)),
    bundle_bytes: new TextEncoder().encode(last.candidate.bundle).byteLength,
    source_map_bytes: new TextEncoder().encode(last.candidate.source_map)
      .byteLength,
  });
}
console.log(JSON.stringify(results, null, 2));

function graph(moduleCount: number, value: number): Record<string, string> {
  const files: Record<string, string> = {
    "/App.ts":
      `import { value } from "./Module000.ts"; export default () => value;`,
  };
  const leaf = moduleCount - 2;
  for (let index = 0; index <= leaf; index += 1) {
    const path = `/Module${pad(index)}.ts`;
    files[path] = index === leaf
      ? `export const value = ${value};`
      : `export { value } from "./Module${pad(index + 1)}.ts";`;
  }
  return files;
}

function request(
  revision: number,
  files: Record<string, string>,
): CompileRequest {
  return {
    type: "compile",
    request_id: `benchmark-${revision}`,
    source_revision: revision,
    entrypoint: "/App.ts",
    files,
  };
}

async function measured<T>(run: () => Promise<T>): Promise<{
  value: T;
  elapsedMs: number;
}> {
  const startedAt = performance.now();
  const value = await run();
  return { value, elapsedMs: performance.now() - startedAt };
}

function percentile(values: number[], ratio: number): number {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[
    Math.min(sorted.length - 1, Math.ceil(ratio * sorted.length) - 1)
  ];
}

function pad(value: number): string {
  return String(value).padStart(3, "0");
}

function round(value: number): number {
  return Math.round(value * 10) / 10;
}
