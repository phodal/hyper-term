export const MAX_RUNTIME_ERROR_MESSAGE_BYTES = 16 * 1024;
export const MAX_RUNTIME_ERROR_STACK_BYTES = 64 * 1024;

export interface GeneratedRuntimePosition {
  line: number;
  column: number;
}

export function generatedPositionFromStack(
  stack: string,
): GeneratedRuntimePosition | undefined {
  const bounded = boundedRuntimeText(stack, MAX_RUNTIME_ERROR_STACK_BYTES);
  const location = /(?:\(|\s|@)((?:blob:|https?:\/\/)[^\s)]+):(\d+):(\d+)\)?/g;
  for (const match of bounded.matchAll(location)) {
    const line = Number(match[2]);
    const column = Number(match[3]);
    if (
      Number.isSafeInteger(line) && Number.isSafeInteger(column) && line > 0 &&
      column > 0
    ) {
      return { line, column };
    }
  }
  return undefined;
}

export function boundedRuntimeText(value: string, maximum: number): string {
  const sanitized = value.replace(
    // deno-lint-ignore no-control-regex -- C0 controls are untrusted preview data.
    /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g,
    " ",
  );
  const encoded = new TextEncoder().encode(sanitized);
  return encoded.byteLength <= maximum
    ? sanitized
    : new TextDecoder().decode(encoded.subarray(0, maximum));
}
