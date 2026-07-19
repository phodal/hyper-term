import { assertEquals } from "@std/assert";
import {
  DEFAULT_TERMINAL_FONT_SIZE,
  MAX_TERMINAL_FONT_SIZE,
  MIN_TERMINAL_FONT_SIZE,
  nextTerminalFontSize,
  parseTerminalFontSize,
  type TerminalKeyDescriptor,
  terminalShortcut,
} from "./terminal-preferences.ts";

const key = (
  value: string,
  overrides: Partial<TerminalKeyDescriptor> = {},
): TerminalKeyDescriptor => ({
  key: value,
  metaKey: true,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...overrides,
});

Deno.test("terminal shortcuts claim find and zoom but leave shell keys alone", () => {
  assertEquals(terminalShortcut(key("f")), "find");
  assertEquals(terminalShortcut(key("+", { shiftKey: true })), "zoom_in");
  assertEquals(terminalShortcut(key("-")), "zoom_out");
  assertEquals(terminalShortcut(key("0")), "zoom_reset");
  assertEquals(terminalShortcut(key("w")), null);
  assertEquals(
    terminalShortcut(key("c", { metaKey: false, ctrlKey: true })),
    null,
  );
  assertEquals(terminalShortcut(key("f", { shiftKey: true })), null);
});

Deno.test("terminal font preference accepts only the bounded persisted form", () => {
  assertEquals(parseTerminalFontSize("15"), 15);
  assertEquals(parseTerminalFontSize("08"), DEFAULT_TERMINAL_FONT_SIZE);
  assertEquals(parseTerminalFontSize("25"), DEFAULT_TERMINAL_FONT_SIZE);
  assertEquals(parseTerminalFontSize("13.5"), DEFAULT_TERMINAL_FONT_SIZE);
  assertEquals(parseTerminalFontSize(null), DEFAULT_TERMINAL_FONT_SIZE);
});

Deno.test("terminal zoom steps clamp and reset deterministically", () => {
  assertEquals(nextTerminalFontSize(13, "zoom_in"), 14);
  assertEquals(
    nextTerminalFontSize(MAX_TERMINAL_FONT_SIZE, "zoom_in"),
    MAX_TERMINAL_FONT_SIZE,
  );
  assertEquals(
    nextTerminalFontSize(MIN_TERMINAL_FONT_SIZE, "zoom_out"),
    MIN_TERMINAL_FONT_SIZE,
  );
  assertEquals(
    nextTerminalFontSize(21, "zoom_reset"),
    DEFAULT_TERMINAL_FONT_SIZE,
  );
});
