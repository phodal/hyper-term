import { assertEquals, assertNotEquals } from "@std/assert";
import {
  darkTerminalTheme,
  lightTerminalTheme,
  terminalColorScheme,
  terminalTheme,
} from "./terminal-theme.ts";

Deno.test("terminal theme follows the system color scheme", () => {
  assertEquals(terminalColorScheme(false), "dark");
  assertEquals(terminalColorScheme(true), "light");
  assertEquals(terminalTheme("dark"), darkTerminalTheme);
  assertEquals(terminalTheme("light"), lightTerminalTheme);
  assertNotEquals(darkTerminalTheme.background, lightTerminalTheme.background);
  assertNotEquals(darkTerminalTheme.foreground, lightTerminalTheme.foreground);
});

Deno.test("terminal themes keep semantic status colors distinct", () => {
  for (const theme of [darkTerminalTheme, lightTerminalTheme]) {
    assertNotEquals(theme.red, theme.green);
    assertNotEquals(theme.green, theme.yellow);
    assertNotEquals(theme.yellow, theme.blue);
    assertNotEquals(theme.cursor, theme.background);
  }
});
