import { assertEquals, assertMatch } from "@std/assert";
import {
  generatedPositionFromStack,
  mapPreviewRuntimeError,
  MAX_RUNTIME_ERROR_STACK_BYTES,
  MAX_RUNTIME_SOURCE_MAP_BYTES,
} from "./runtime-diagnostic.ts";

function sourceMap(source = "hyper-vfs:/App.tsx"): string {
  return JSON.stringify({
    version: 3,
    sources: [source],
    names: [],
    mappings: "AAAA;AACA",
    sourcesContent: ["first();\nsecond();"],
  });
}

Deno.test("runtime errors map blob positions to accepted virtual source", () => {
  const diagnostic = mapPreviewRuntimeError({
    message: "render failed",
    stack: "Error: render failed\n    at App (blob:http://127.0.0.1/abc:2:1)",
  }, sourceMap());

  assertEquals(diagnostic.generated, { line: 2, column: 1 });
  assertEquals(diagnostic.original, { file: "/App.tsx", line: 2, column: 1 });
});

Deno.test("preview supplied positions take precedence over later stack frames", () => {
  const diagnostic = mapPreviewRuntimeError({
    message: "event failed",
    stack: "at later (blob:http://127.0.0.1/abc:2:1)",
    generated_line: 1,
    generated_column: 1,
  }, sourceMap());

  assertEquals(diagnostic.original, { file: "/App.tsx", line: 1, column: 1 });
});

Deno.test("runtime mapper rejects capsule and oversized source maps", () => {
  assertEquals(
    mapPreviewRuntimeError({
      message: "capsule failed",
      generated_line: 1,
      generated_column: 1,
    }, sourceMap("hyper-capsule:react")).original,
    undefined,
  );
  assertEquals(
    mapPreviewRuntimeError({
      message: "large map",
      generated_line: 1,
      generated_column: 1,
    }, "x".repeat(MAX_RUNTIME_SOURCE_MAP_BYTES + 1)).original,
    undefined,
  );
});

Deno.test("runtime stacks and control characters stay bounded", () => {
  const stack = `Error:\u0000bad\n at App (blob:http://local/id:8:13)${
    "x".repeat(MAX_RUNTIME_ERROR_STACK_BYTES)
  }`;
  const diagnostic = mapPreviewRuntimeError({
    message: "bad\u0000message",
    stack,
  }, "{}");

  assertEquals(generatedPositionFromStack(stack), { line: 8, column: 13 });
  assertMatch(diagnostic.message, /^bad message$/);
  if (!diagnostic.stack) throw new Error("bounded stack missing");
  assertEquals(
    new TextEncoder().encode(diagnostic.stack).byteLength,
    MAX_RUNTIME_ERROR_STACK_BYTES,
  );
});
