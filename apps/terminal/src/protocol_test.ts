import { assertEquals, assertThrows } from "@std/assert";
import {
  decodeTerminalBinary,
  encodeTerminalInput,
  MAX_TERMINAL_WEB_PAYLOAD_BYTES,
  TerminalWebBinaryKind,
} from "./protocol.ts";

Deno.test("terminal input uses the Rust-compatible binary envelope", () => {
  const encoded = encodeTerminalInput(42n, new Uint8Array([0, 27, 255]));
  const decoded = decodeTerminalBinary(encoded);

  assertEquals(
    [...new Uint8Array(encoded)]
      .map((byte) => byte.toString(16).padStart(2, "0"))
      .join(""),
    "4854575300010100000000000000002a0000000000000000000000000000000000000003001bff",
  );

  assertEquals(decoded, {
    kind: TerminalWebBinaryKind.Input,
    sequence: 42n,
    bytes: new Uint8Array([0, 27, 255]),
  });
});

Deno.test("terminal protocol rejects oversized browser input", () => {
  assertThrows(
    () =>
      encodeTerminalInput(
        1n,
        new Uint8Array(MAX_TERMINAL_WEB_PAYLOAD_BYTES + 1),
      ),
    Error,
    "too large",
  );
});

Deno.test("terminal protocol rejects a truncated frame", () => {
  assertThrows(
    () => decodeTerminalBinary(new ArrayBuffer(8)),
    Error,
    "truncated",
  );
});
