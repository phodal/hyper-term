import { originalPositionFor, TraceMap } from "@jridgewell/trace-mapping";
import {
  boundedRuntimeText,
  generatedPositionFromStack,
  MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  MAX_RUNTIME_ERROR_STACK_BYTES,
} from "../../genui/runtime-error.ts";

export {
  generatedPositionFromStack,
  MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  MAX_RUNTIME_ERROR_STACK_BYTES,
} from "../../genui/runtime-error.ts";
export const MAX_RUNTIME_SOURCE_MAP_BYTES = 768 * 1024;

export interface RuntimeSourceLocation {
  file: string;
  line: number;
  column: number;
}

export interface RuntimeDiagnostic {
  message: string;
  stack?: string;
  generated?: Omit<RuntimeSourceLocation, "file">;
  original?: RuntimeSourceLocation;
}

export interface PreviewRuntimeError {
  message?: unknown;
  stack?: unknown;
  generated_line?: unknown;
  generated_column?: unknown;
}

export function mapPreviewRuntimeError(
  error: PreviewRuntimeError,
  sourceMap: string,
): RuntimeDiagnostic {
  const message = boundedRuntimeText(
    typeof error.message === "string" ? error.message : "Unknown runtime error",
    MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  );
  const stack = typeof error.stack === "string"
    ? boundedRuntimeText(error.stack, MAX_RUNTIME_ERROR_STACK_BYTES)
    : undefined;
  const generated = validPosition(
    error.generated_line,
    error.generated_column,
  ) ?? (stack ? generatedPositionFromStack(stack) : undefined);
  const original = generated
    ? mapGeneratedPosition(sourceMap, generated.line, generated.column)
    : undefined;
  return { message, stack, generated, original };
}

function mapGeneratedPosition(
  sourceMap: string,
  line: number,
  column: number,
): RuntimeSourceLocation | undefined {
  if (
    new TextEncoder().encode(sourceMap).byteLength >
      MAX_RUNTIME_SOURCE_MAP_BYTES
  ) {
    return undefined;
  }
  try {
    const mapped = originalPositionFor(new TraceMap(sourceMap), {
      line,
      column: column - 1,
    });
    if (
      mapped.line === null || mapped.column === null || mapped.source === null
    ) {
      return undefined;
    }
    const file = trustedVirtualSource(mapped.source);
    if (!file) return undefined;
    return { file, line: mapped.line, column: mapped.column + 1 };
  } catch {
    return undefined;
  }
}

function trustedVirtualSource(source: string): string | undefined {
  const prefix = "hyper-vfs:";
  if (!source.startsWith(prefix)) return undefined;
  const file = source.slice(prefix.length);
  if (
    !file.startsWith("/") || file.includes("..") || file.startsWith("/__hyper_")
  ) {
    return undefined;
  }
  return file;
}

function validPosition(
  line: unknown,
  column: unknown,
): Omit<RuntimeSourceLocation, "file"> | undefined {
  return validLineColumn(line, column)
    ? { line: Number(line), column: Number(column) }
    : undefined;
}

function validLineColumn(
  line: unknown,
  column: unknown,
): boolean {
  return Number.isSafeInteger(line) && Number.isSafeInteger(column) &&
    Number(line) > 0 && Number(column) > 0;
}
