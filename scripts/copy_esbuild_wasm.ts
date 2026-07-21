import { dirname, fromFileUrl } from "@std/path";

const resolved = import.meta.resolve("npm:esbuild-wasm@0.28.1/esbuild.wasm");
if (!resolved.startsWith("file:")) {
  throw new Error(`esbuild-wasm resolved to an unsupported URL: ${resolved}`);
}

const target = Deno.args[0];
const destinationUrl = target === "workbench"
  ? new URL("../dist/workbench/esbuild.wasm", import.meta.url)
  : target === "runtime"
  ? new URL("../dist/runtime/esbuild.wasm", import.meta.url)
  : target === "test"
  ? new URL("../.zig-cache/test-runtime/esbuild.wasm", import.meta.url)
  : null;
if (destinationUrl === null) {
  throw new Error(
    "copy_esbuild_wasm target must be workbench, runtime, or test",
  );
}
const destination = fromFileUrl(destinationUrl);
await Deno.mkdir(dirname(destination), { recursive: true });
await Deno.copyFile(fromFileUrl(resolved), destination);
