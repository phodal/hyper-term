import type { ITheme } from "@xterm/xterm";

export type TerminalColorScheme = "dark" | "light";

export const darkTerminalTheme: ITheme = {
  background: "#0d0f0b",
  foreground: "#e6e9dd",
  cursor: "#d7ff72",
  cursorAccent: "#11140d",
  selectionBackground: "#465a2b",
  selectionForeground: "#ffffff",
  black: "#0d0f0b",
  red: "#ff8d83",
  green: "#9bcf5d",
  yellow: "#f0bf68",
  blue: "#8bc6ff",
  magenta: "#d5a6ff",
  cyan: "#7dd7d0",
  white: "#e6e9dd",
  brightBlack: "#89917e",
  brightRed: "#ffb3ac",
  brightGreen: "#d7ff72",
  brightYellow: "#ffd797",
  brightBlue: "#b9ddff",
  brightMagenta: "#e9cfff",
  brightCyan: "#a9fff5",
  brightWhite: "#ffffff",
};

export const lightTerminalTheme: ITheme = {
  background: "#f7f9f1",
  foreground: "#171a14",
  cursor: "#456109",
  cursorAccent: "#f7ffd9",
  selectionBackground: "#e1e7d4",
  selectionForeground: "#171a14",
  black: "#171a14",
  red: "#a6312b",
  green: "#37670d",
  yellow: "#8a5500",
  blue: "#185e8b",
  magenta: "#6f438b",
  cyan: "#176b68",
  white: "#171a14",
  brightBlack: "#626a5b",
  brightRed: "#c4473f",
  brightGreen: "#456109",
  brightYellow: "#a86d12",
  brightBlue: "#2779aa",
  brightMagenta: "#8c5aaa",
  brightCyan: "#237f7b",
  brightWhite: "#000000",
};

export function terminalColorScheme(
  prefersLight: boolean,
): TerminalColorScheme {
  return prefersLight ? "light" : "dark";
}

export function terminalTheme(scheme: TerminalColorScheme): ITheme {
  return scheme === "light" ? lightTerminalTheme : darkTerminalTheme;
}
