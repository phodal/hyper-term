import type { ArtifactCandidate } from "../protocol.ts";
import {
  type CompileRequest,
  type CompileResponse,
  validateCompileRequest,
} from "./compiler-protocol.ts";

interface PendingCompile {
  revision: number;
  resolve: (candidate: ArtifactCandidate) => void;
  reject: (error: Error) => void;
  timeout: number;
}

export class GenUiCompiler {
  readonly #worker: Worker;
  readonly #pending = new Map<string, PendingCompile>();
  #latestRevision = 0;

  constructor() {
    this.#worker = new Worker(
      new URL("./compiler.worker.js", document.baseURI),
      { type: "module", name: "hyper-term-genui-compiler" },
    );
    this.#worker.onmessage = (message: MessageEvent<CompileResponse>) =>
      this.#receive(message.data);
    this.#worker.onerror = (event) => {
      this.#failAll(new Error(event.message || "compiler worker failed"));
    };
  }

  compile(
    sourceRevision: number,
    entrypoint: string,
    files: Record<string, string>,
  ): Promise<ArtifactCandidate> {
    this.#latestRevision = Math.max(this.#latestRevision, sourceRevision);
    const requestId = crypto.randomUUID();
    const request = createCompileRequest(
      requestId,
      sourceRevision,
      entrypoint,
      files,
    );
    return new Promise((resolve, reject) => {
      const timeout = globalThis.setTimeout(() => {
        this.#pending.delete(requestId);
        reject(new Error("Agentic UI compilation timed out"));
      }, 10_000);
      this.#pending.set(requestId, {
        revision: sourceRevision,
        resolve,
        reject,
        timeout,
      });
      this.#worker.postMessage(request);
    });
  }

  dispose(): void {
    this.#worker.terminate();
    this.#failAll(new Error("compiler was disposed"));
  }

  #receive(response: CompileResponse): void {
    const pending = this.#pending.get(response.request_id);
    if (!pending) return;
    this.#pending.delete(response.request_id);
    clearTimeout(pending.timeout);
    if (pending.revision < this.#latestRevision) {
      pending.reject(new Error("stale compile result was discarded"));
      return;
    }
    if (response.type === "compile_failed") {
      pending.reject(
        new Error(response.diagnostics.map((item) => item.text).join("\n")),
      );
      return;
    }
    pending.resolve(response.candidate);
  }

  #failAll(error: Error): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.#pending.clear();
  }
}

export function createCompileRequest(
  requestId: string,
  sourceRevision: number,
  entrypoint: string,
  files: Record<string, string>,
): CompileRequest {
  const request: CompileRequest = {
    type: "compile",
    request_id: requestId,
    source_revision: sourceRevision,
    entrypoint,
    files: { ...files },
  };
  validateCompileRequest(request);
  return request;
}
