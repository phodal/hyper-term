export const TERMINAL_WEB_PROTOCOL_VERSION = 1;
export const TERMINAL_WEB_HEADER_BYTES = 36;
export const MAX_TERMINAL_WEB_PAYLOAD_BYTES = 256 * 1024;

const MAGIC = new Uint8Array([0x48, 0x54, 0x57, 0x53]); // HTWS

export const enum TerminalWebBinaryKind {
  Input = 1,
  Output = 2,
  Snapshot = 3,
}

export interface TerminalSize {
  cols: number;
  rows: number;
  pixel_width: number;
  pixel_height: number;
}

export type TerminalWebClientControl =
  | {
    type: "hello";
    protocol_version: number;
    attachment_id: string | null;
    after_sequence: number;
    size: TerminalSize;
    cwd: string | null;
  }
  | { type: "resize"; generation: number; size: TerminalSize }
  | { type: "close" };

export type TerminalWebServerControl =
  | {
    type: "ready";
    protocol_version: number;
    attachment_id: string;
    terminal_id: string;
    next_input_sequence: number;
    resize_generation: number;
  }
  | { type: "exited"; exit_code: number | null; signal: string | null }
  | { type: "error"; code: string; message: string };

export type TerminalWebBinaryFrame =
  | { kind: TerminalWebBinaryKind.Input; sequence: bigint; bytes: Uint8Array }
  | { kind: TerminalWebBinaryKind.Output; sequence: bigint; bytes: Uint8Array }
  | {
    kind: TerminalWebBinaryKind.Snapshot;
    baseSequence: bigint;
    nextSequence: bigint;
    totalBytes: bigint;
    bytes: Uint8Array;
  };

export function encodeTerminalInput(
  sequence: bigint,
  payload: Uint8Array,
): ArrayBuffer {
  if (payload.byteLength > MAX_TERMINAL_WEB_PAYLOAD_BYTES) {
    throw new Error(
      `terminal input payload is too large: ${payload.byteLength}`,
    );
  }
  const encoded = new ArrayBuffer(
    TERMINAL_WEB_HEADER_BYTES + payload.byteLength,
  );
  const bytes = new Uint8Array(encoded);
  const view = new DataView(encoded);
  bytes.set(MAGIC, 0);
  view.setUint16(4, TERMINAL_WEB_PROTOCOL_VERSION);
  view.setUint8(6, TerminalWebBinaryKind.Input);
  view.setUint8(7, 0);
  view.setBigUint64(8, sequence);
  view.setBigUint64(16, 0n);
  view.setBigUint64(24, 0n);
  view.setUint32(32, payload.byteLength);
  bytes.set(payload, TERMINAL_WEB_HEADER_BYTES);
  return encoded;
}

export function decodeTerminalBinary(
  encoded: ArrayBuffer,
): TerminalWebBinaryFrame {
  if (encoded.byteLength < TERMINAL_WEB_HEADER_BYTES) {
    throw new Error("truncated terminal WebSocket frame");
  }
  const bytes = new Uint8Array(encoded);
  if (!MAGIC.every((byte, index) => bytes[index] === byte)) {
    throw new Error("invalid terminal WebSocket frame magic");
  }
  const view = new DataView(encoded);
  const version = view.getUint16(4);
  if (version !== TERMINAL_WEB_PROTOCOL_VERSION) {
    throw new Error(`unsupported terminal WebSocket protocol ${version}`);
  }
  const kind = view.getUint8(6) as TerminalWebBinaryKind;
  if (view.getUint8(7) !== 0) {
    throw new Error("terminal WebSocket frame contains unsupported flags");
  }
  const payloadLength = view.getUint32(32);
  if (payloadLength > MAX_TERMINAL_WEB_PAYLOAD_BYTES) {
    throw new Error(`terminal output payload is too large: ${payloadLength}`);
  }
  if (payloadLength !== encoded.byteLength - TERMINAL_WEB_HEADER_BYTES) {
    throw new Error("terminal WebSocket frame payload length does not match");
  }
  const payload = bytes.slice(TERMINAL_WEB_HEADER_BYTES);
  const primary = view.getBigUint64(8);
  const secondary = view.getBigUint64(16);
  const totalBytes = view.getBigUint64(24);
  if (kind === TerminalWebBinaryKind.Input) {
    assertUnusedMetadata(kind, secondary, totalBytes);
    return { kind, sequence: primary, bytes: payload };
  }
  if (kind === TerminalWebBinaryKind.Output) {
    assertUnusedMetadata(kind, secondary, totalBytes);
    return { kind, sequence: primary, bytes: payload };
  }
  if (kind === TerminalWebBinaryKind.Snapshot) {
    return {
      kind,
      baseSequence: primary,
      nextSequence: secondary,
      totalBytes,
      bytes: payload,
    };
  }
  throw new Error(`unknown terminal WebSocket frame kind ${kind}`);
}

function assertUnusedMetadata(
  kind: TerminalWebBinaryKind,
  secondary: bigint,
  totalBytes: bigint,
): void {
  if (secondary !== 0n || totalBytes !== 0n) {
    throw new Error(
      `terminal WebSocket frame kind ${kind} has unexpected metadata`,
    );
  }
}
