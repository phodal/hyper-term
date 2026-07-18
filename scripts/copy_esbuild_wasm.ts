import { dirname, fromFileUrl } from "@std/path";

const resolved = import.meta.resolve("npm:esbuild-wasm@0.28.1/esbuild.wasm");
if (!resolved.startsWith("file:")) {
  throw new Error(`esbuild-wasm resolved to an unsupported URL: ${resolved}`);
}

const destination = fromFileUrl(
  new URL("../dist/workbench/esbuild.wasm", import.meta.url),
);
await Deno.mkdir(dirname(destination), { recursive: true });
await Deno.copyFile(fromFileUrl(resolved), destination);
