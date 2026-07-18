import {
  type CompileRequest,
  type CompileResponse,
} from "./compiler-protocol.ts";
import {
  compileRequest,
  diagnosticsFrom,
  initializeCompiler,
} from "./compiler-engine.ts";
import * as esbuild from "esbuild-wasm";

globalThis.onmessage = (message: MessageEvent<CompileRequest>) => {
  void compile(message.data).then(post).catch((error: unknown) => {
    post({
      type: "compile_failed",
      request_id: message.data.request_id,
      source_revision: message.data.source_revision,
      diagnostics: diagnosticsFrom(error),
    });
  });
};

async function compile(request: CompileRequest): Promise<CompileResponse> {
  await initializeCompiler(esbuild, {
    wasmURL: new URL("./esbuild.wasm", globalThis.location.href).href,
    worker: false,
  });
  return await compileRequest(request);
}

function post(response: CompileResponse): void {
  globalThis.postMessage(response);
}
