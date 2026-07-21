import {
  type CompileRequest,
  type CompileResponse,
} from "./compiler-protocol.ts";
import {
  cancelCompiler,
  compileRequest,
  diagnosticsFrom,
  initializeCompiler,
} from "./compiler-engine.ts";
import {
  cancelPreviewSliceCompiler,
  canUsePreviewSliceCompiler,
  compilePreviewSlice,
} from "./preview-slice-compiler.ts";
import { LatestCompileScheduler } from "./compiler-scheduler.ts";
import * as esbuild from "esbuild-wasm";

globalThis.onmessage = (message: MessageEvent<CompileRequest>) => {
  scheduler.enqueue(message.data);
};

const scheduler = new LatestCompileScheduler(
  async (request) => {
    try {
      return await compile(request);
    } catch (error) {
      const response: CompileResponse = {
        type: "compile_failed",
        request_id: request.request_id,
        source_revision: request.source_revision,
        diagnostics: diagnosticsFrom(error),
      };
      return response;
    }
  },
  post,
  async () => {
    cancelPreviewSliceCompiler();
    await cancelCompiler();
  },
);

async function compile(request: CompileRequest): Promise<CompileResponse> {
  await initializeCompiler(esbuild, {
    wasmURL: new URL("./esbuild.wasm", globalThis.location.href).href,
    worker: false,
  });
  return canUsePreviewSliceCompiler(request)
    ? await compilePreviewSlice(request, esbuild)
    : await compileRequest(request);
}

function post(response: CompileResponse): void {
  globalThis.postMessage(response);
}
