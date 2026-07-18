import { assertThrows } from "@std/assert";
import {
  type CompileRequest,
  MAX_SOURCE_BYTES,
  validateCompileRequest,
} from "./compiler-protocol.ts";

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
