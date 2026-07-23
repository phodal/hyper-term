import { assertEquals, assertThrows } from "@std/assert";
import {
  buildWebRendererManifest,
  type WebRendererFile,
} from "./package_web_renderers.ts";

const requiredFiles: WebRendererFile[] = [
  "README.md",
  "index.html",
  "terminal/index.html",
  "terminal/build-manifest.json",
  "workbench/index.html",
  "workbench/build-manifest.json",
  "workbench/compiler.worker.js",
  "workbench/esbuild.wasm",
  "workbench/genui/preview.html",
].map((path, index) => ({
  path,
  bytes: index + 1,
  sha256: `${index}`.padStart(64, "0"),
}));

Deno.test("web renderer package exposes the honest host contract", () => {
  const manifest = buildWebRendererManifest("0.1.0", [
    ...requiredFiles.toReversed(),
  ]);
  assertEquals(manifest.product, "hyper-term-web-renderers");
  assertEquals(manifest.host_contract, {
    native_sdk_web_target: false,
    terminal_requires_gateway: true,
    workbench_standalone_demo: true,
    authoritative_operations_require_rust: true,
  });
  assertEquals(manifest.entries.compiler_wasm, "workbench/esbuild.wasm");
  assertEquals(
    manifest.files.map((file) => file.path),
    [...requiredFiles].map((file) => file.path).sort((left, right) =>
      left.localeCompare(right)
    ),
  );
});

Deno.test("web renderer manifest rejects an incomplete package", () => {
  assertThrows(
    () => buildWebRendererManifest("0.1.0", requiredFiles.slice(1)),
    Error,
    "README.md",
  );
  assertThrows(
    () => buildWebRendererManifest("0.1.0-rc.1", requiredFiles),
    Error,
    "invalid Hyper Term version",
  );
});
