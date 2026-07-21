import { assertEquals } from "@std/assert";
import type { CompileRequest, CompileResponse } from "./compiler-protocol.ts";
import { LatestCompileScheduler } from "./compiler-scheduler.ts";

function request(revision: number): CompileRequest {
  return {
    type: "compile",
    request_id: `request-${revision}`,
    source_revision: revision,
    entrypoint: "/App.tsx",
    files: { "/App.tsx": `export default ${revision}` },
  };
}

function failed(request: CompileRequest): CompileResponse {
  return {
    type: "compile_failed",
    request_id: request.request_id,
    source_revision: request.source_revision,
    diagnostics: [{ severity: "error", text: "test response" }],
  };
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => resolve = next);
  return { promise, resolve };
}

Deno.test("compiler scheduler runs one build and keeps only the latest queued edit", async () => {
  const first = deferred<CompileResponse>();
  const latest = deferred<CompileResponse>();
  const started: number[] = [];
  const posted: CompileResponse[] = [];
  let cancellations = 0;
  const scheduler = new LatestCompileScheduler(
    (compileRequest) => {
      started.push(compileRequest.source_revision);
      return compileRequest.source_revision === 1
        ? first.promise
        : latest.promise;
    },
    (response) => posted.push(response),
    () => {
      cancellations += 1;
      return Promise.resolve();
    },
  );

  scheduler.enqueue(request(1));
  scheduler.enqueue(request(2));
  scheduler.enqueue(request(3));

  assertEquals(started, [1]);
  assertEquals(cancellations, 1);
  assertEquals(posted, [{
    type: "compile_superseded",
    request_id: "request-2",
    source_revision: 2,
    superseded_by_request_id: "request-3",
    superseded_by_source_revision: 3,
  }]);

  first.resolve(failed(request(1)));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assertEquals(started, [1, 3]);

  latest.resolve(failed(request(3)));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assertEquals(posted.map((response) => response.source_revision), [2, 1, 3]);
});
