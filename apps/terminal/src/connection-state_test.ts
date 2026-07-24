import { assertEquals, assertThrows } from "@std/assert";
import {
  terminalAttachmentStorageKey,
  TerminalConnectionState,
  terminalReconnectPresentation,
  terminalSessionId,
} from "./connection-state.ts";

Deno.test("an open socket cannot send terminal data before protocol ready", () => {
  const state = new TerminalConnectionState();
  state.beginConnection();

  assertEquals(state.canSend(true), false);
  assertThrows(() => state.takeInputSequence(), Error, "not ready");
  assertThrows(() => state.takeResizeGeneration(), Error, "not ready");
});

Deno.test("an attached terminal reconnects quietly while a cold failure stays visible", () => {
  assertEquals(terminalReconnectPresentation(true, 300), {
    message: "Reattaching…",
    visible: false,
  });
  assertEquals(terminalReconnectPresentation(false, 300), {
    message: "Disconnected · retrying in 0.3s",
    visible: true,
  });
});

Deno.test("terminal tabs keep independent reconnect attachments", () => {
  const first = terminalAttachmentStorageKey(
    "http://127.0.0.1:47437/?token=x&tab=1",
  );
  const second = terminalAttachmentStorageKey(
    "http://127.0.0.1:47437/?token=x&tab=2",
  );

  assertEquals(first, "hyper-term.terminal-attachment.v1.tab-1");
  assertEquals(second, "hyper-term.terminal-attachment.v1.tab-2");
  assertEquals(
    terminalAttachmentStorageKey("http://127.0.0.1:47437/?token=x&tab=invalid"),
    "hyper-term.terminal-attachment.v1",
  );
  assertEquals(terminalSessionId("http://127.0.0.1:47437/?token=x&tab=2"), 2);
  assertEquals(
    terminalSessionId("http://127.0.0.1:47437/?token=x&tab=1000"),
    null,
  );
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
