export const DEFAULT_TERMINAL_FONT_SIZE = 13;
export const MIN_TERMINAL_FONT_SIZE = 9;
export const MAX_TERMINAL_FONT_SIZE = 24;
export const TERMINAL_FONT_SIZE_STORAGE_KEY =
  "hyper-term.terminal-font-size.v1";

export type TerminalShortcut =
  | "find"
  | "zoom_in"
  | "zoom_out"
  | "zoom_reset";

export interface TerminalKeyDescriptor {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export function terminalShortcut(
  event: TerminalKeyDescriptor,
): TerminalShortcut | null {
  if (!event.metaKey || event.ctrlKey || event.altKey) return null;
  const key = event.key.toLowerCase();
  if (key === "f" && !event.shiftKey) return "find";
  if (key === "0" && !event.shiftKey) return "zoom_reset";
  if (key === "+" || key === "=") return "zoom_in";
  if (key === "-" || key === "_") return "zoom_out";
  return null;
}

export function parseTerminalFontSize(value: string | null): number {
  if (value === null || !/^[0-9]{1,2}$/.test(value)) {
    return DEFAULT_TERMINAL_FONT_SIZE;
  }
  const parsed = Number(value);
  return parsed >= MIN_TERMINAL_FONT_SIZE && parsed <= MAX_TERMINAL_FONT_SIZE
    ? parsed
    : DEFAULT_TERMINAL_FONT_SIZE;
}

export function nextTerminalFontSize(
  current: number,
  shortcut: Extract<TerminalShortcut, "zoom_in" | "zoom_out" | "zoom_reset">,
): number {
  if (shortcut === "zoom_reset") return DEFAULT_TERMINAL_FONT_SIZE;
  const bounded = Math.max(
    MIN_TERMINAL_FONT_SIZE,
    Math.min(MAX_TERMINAL_FONT_SIZE, Math.round(current)),
  );
  return Math.max(
    MIN_TERMINAL_FONT_SIZE,
    Math.min(
      MAX_TERMINAL_FONT_SIZE,
      bounded + (shortcut === "zoom_in" ? 1 : -1),
    ),
  );
}
