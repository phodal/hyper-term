import type {
  CompileRequest,
  CompileResponse,
} from "../apps/workbench/src/genui/compiler-protocol.ts";
// @ts-types="esbuild-wasm/browser-types"
import * as esbuild from "esbuild-wasm/browser";
import {
  compileRequest,
  diagnosticsFrom,
  initializeCompiler,
} from "../apps/workbench/src/genui/compiler-engine.ts";

const MAX_INPUT_BYTES = 3 * 1024 * 1024;
const wasmPath = requiredWasmPath(Deno.args);
const wasmBytes = await Deno.readFile(wasmPath);
const wasmModule = await WebAssembly.compile(wasmBytes);
await initializeCompiler(esbuild, { wasmModule, worker: false });
await write({
  type: "ready",
  protocol_version: 1,
  compiler: { name: "esbuild-wasm", version: esbuild.version },
});

const decoder = new TextDecoder();
let buffered = "";
for await (const chunk of Deno.stdin.readable) {
  buffered += decoder.decode(chunk, { stream: true });
  if (new TextEncoder().encode(buffered).byteLength > MAX_INPUT_BYTES) {
    await write({
      type: "protocol_error",
      message: "compiler input exceeds its bound",
    });
    Deno.exit(2);
  }
  let newline = buffered.indexOf("\n");
  while (newline >= 0) {
    const line = buffered.slice(0, newline).trim();
    buffered = buffered.slice(newline + 1);
    if (line.length > 0) await handle(line);
    newline = buffered.indexOf("\n");
  }
}
buffered += decoder.decode();
if (buffered.trim().length > 0) await handle(buffered.trim());

async function handle(line: string): Promise<void> {
  let request: CompileRequest;
  try {
    request = JSON.parse(line) as CompileRequest;
  } catch {
    await write({
      type: "protocol_error",
      message: "compiler request is not valid JSON",
    });
    return;
  }
  try {
    await write(await compileRequest(request));
  } catch (error) {
    const response: CompileResponse = {
      type: "compile_failed",
      request_id: typeof request.request_id === "string"
        ? request.request_id.slice(0, 128)
        : "invalid-request",
      source_revision: Number.isSafeInteger(request.source_revision)
        ? request.source_revision
        : 0,
      diagnostics: diagnosticsFrom(error),
    };
    await write(response);
  }
}

async function write(value: unknown): Promise<void> {
  const bytes = new TextEncoder().encode(`${JSON.stringify(value)}\n`);
  await Deno.stdout.write(bytes);
}

function requiredWasmPath(arguments_: string[]): string {
  if (arguments_.length !== 2 || arguments_[0] !== "--wasm") {
    throw new Error(
      "usage: genui-compiler --wasm /absolute/path/to/esbuild.wasm",
    );
  }
  const path = arguments_[1];
  if (!path.startsWith("/")) {
    throw new Error("esbuild.wasm path must be absolute");
  }
  return path;
}
