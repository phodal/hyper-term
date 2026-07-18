import type { ArtifactCandidate, CompileDiagnostic } from "../protocol.ts";

export const MAX_SOURCE_FILES = 100;
export const MAX_SOURCE_BYTES = 2 * 1024 * 1024;

export interface CompileRequest {
  type: "compile";
  request_id: string;
  source_revision: number;
  entrypoint: string;
  files: Record<string, string>;
}

export type CompileResponse =
  | {
    type: "compiled";
    request_id: string;
    source_revision: number;
    candidate: ArtifactCandidate;
  }
  | {
    type: "compile_failed";
    request_id: string;
    source_revision: number;
    diagnostics: CompileDiagnostic[];
  };

export function validateCompileRequest(request: CompileRequest): void {
  if (request.type !== "compile") {
    throw new Error("compiler request type must be compile");
  }
  if (
    typeof request.request_id !== "string" ||
    request.request_id.length === 0 ||
    request.request_id.length > 128
  ) {
    throw new Error("compiler request id must contain 1-128 characters");
  }
  if (
    !Number.isSafeInteger(request.source_revision) ||
    request.source_revision < 1
  ) {
    throw new Error("source revision must be a positive integer");
  }
  if (typeof request.entrypoint !== "string") {
    throw new Error("compiler entrypoint must be a string");
  }
  if (
    typeof request.files !== "object" || request.files === null ||
    Array.isArray(request.files)
  ) {
    throw new Error("compiler files must be a virtual source map");
  }
  const entries = Object.entries(request.files);
  if (entries.length === 0 || entries.length > MAX_SOURCE_FILES) {
    throw new Error(`source snapshot must contain 1-${MAX_SOURCE_FILES} files`);
  }
  let bytes = 0;
  for (const [path, source] of entries) {
    if (!path.startsWith("/") || path.includes("..") || path.includes("\\")) {
      throw new Error(`invalid virtual source path: ${path}`);
    }
    bytes += new TextEncoder().encode(source).byteLength;
  }
  if (bytes > MAX_SOURCE_BYTES) {
    throw new Error(`source snapshot exceeds ${MAX_SOURCE_BYTES} bytes`);
  }
  if (!(request.entrypoint in request.files)) {
    throw new Error(`entrypoint ${request.entrypoint} is not in the snapshot`);
  }
}
