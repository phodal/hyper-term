import { assertThrows } from "@std/assert";
import {
  type CompileRequest,
  MAX_SOURCE_BYTES,
  validateCompileRequest,
} from "./compiler-protocol.ts";
import { createCompileRequest } from "./compiler-client.ts";

function request(files: Record<string, string>): CompileRequest {
  return {
    type: "compile",
    request_id: "request-1",
    source_revision: 1,
    entrypoint: "/App.tsx",
    files,
  };
}

Deno.test("compiler accepts only bounded absolute virtual paths", () => {
  assertThrows(
    () => validateCompileRequest(request({ "../App.tsx": "export default 1" })),
    Error,
    "invalid virtual source path",
  );
  assertThrows(
    () =>
      validateCompileRequest(
        request({ "/App.tsx": "x".repeat(MAX_SOURCE_BYTES + 1) }),
      ),
    Error,
    "exceeds",
  );
});

Deno.test("compiler rejects malformed request envelopes", () => {
  assertThrows(
    () =>
      validateCompileRequest({
        ...request({ "/App.tsx": "export default 1" }),
        type: "unknown" as "compile",
      }),
    Error,
    "request type",
  );
  assertThrows(
    () =>
      validateCompileRequest({
        ...request({ "/App.tsx": "export default 1" }),
        request_id: "",
      }),
    Error,
    "request id",
  );
});

Deno.test("compiler client preserves the complete virtual source tree", () => {
  const files = {
    "/App.tsx":
      "import { title } from './title.ts'; export default () => title;",
    "/title.ts": "export const title = 'multi-file';",
  };
  const compiled = createCompileRequest("request-multi", 4, "/App.tsx", files);
  files["/title.ts"] = "mutated after request";

  if (compiled.files["/title.ts"] !== "export const title = 'multi-file';") {
    throw new Error("compile request did not snapshot every virtual file");
  }
  if (Object.keys(compiled.files).length !== 2) {
    throw new Error("compile request dropped a virtual file");
  }
});
