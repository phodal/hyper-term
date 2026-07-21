import type { CompileRequest, CompileResponse } from "./compiler-protocol.ts";

type Compile = (request: CompileRequest) => Promise<CompileResponse>;
type Post = (response: CompileResponse) => void;
type Cancel = () => Promise<void>;

export class LatestCompileScheduler {
  #running = false;
  #queued: CompileRequest | undefined;
  #cancellation: Promise<void> | undefined;

  constructor(
    private readonly compile: Compile,
    private readonly post: Post,
    private readonly cancel: Cancel = () => Promise.resolve(),
  ) {}

  enqueue(request: CompileRequest): void {
    if (!this.#running) {
      this.#running = true;
      void this.#run(request);
      return;
    }
    if (this.#queued) this.post(superseded(this.#queued, request));
    this.#queued = request;
    this.#cancellation ??= this.cancel().catch(() => undefined);
  }

  async #run(request: CompileRequest): Promise<void> {
    this.post(await this.compile(request));
    await this.#cancellation;
    this.#cancellation = undefined;
    const next = this.#queued;
    this.#queued = undefined;
    if (next) {
      void this.#run(next);
    } else {
      this.#running = false;
    }
  }
}

function superseded(
  request: CompileRequest,
  replacement: CompileRequest,
): CompileResponse {
  return {
    type: "compile_superseded",
    request_id: request.request_id,
    source_revision: request.source_revision,
    superseded_by_request_id: replacement.request_id,
    superseded_by_source_revision: replacement.source_revision,
  };
}
