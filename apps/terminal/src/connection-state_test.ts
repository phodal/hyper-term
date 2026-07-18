import { assertEquals, assertThrows } from "@std/assert";
import { TerminalConnectionState } from "./connection-state.ts";

Deno.test("an open socket cannot send terminal data before protocol ready", () => {
  const state = new TerminalConnectionState();
  state.beginConnection();

  assertEquals(state.canSend(true), false);
  assertThrows(() => state.takeInputSequence(), Error, "not ready");
  assertThrows(() => state.takeResizeGeneration(), Error, "not ready");
});

Deno.test("ready sequences advance and are fenced again on reconnect", () => {
  const state = new TerminalConnectionState();
  state.acceptReady(7, 11);

  assertEquals(state.canSend(true), true);
  assertEquals(state.takeInputSequence(), 7n);
  assertEquals(state.takeInputSequence(), 8n);
  assertEquals(state.takeResizeGeneration(), 12);

  state.disconnect();
  assertEquals(state.canSend(true), false);
  state.beginConnection();
  assertEquals(state.canSend(true), false);

  state.acceptReady(23, 31);
  assertEquals(state.takeInputSequence(), 23n);
  assertEquals(state.takeResizeGeneration(), 32);
});
