export const MAX_TERMINAL_TITLE_BYTES = 256;
export const MAX_TERMINAL_CWD_BYTES = 512;

const encoder = new TextEncoder();
export interface TerminalMetadataSnapshot {
  revision: number;
  title: string | null;
  cwd: string | null;
}

export class TerminalMetadataState {
  #revision = 0;
  #title: string | null = null;
  #cwd: string | null = null;

  setTitle(value: string): TerminalMetadataSnapshot | null {
    const next = normalizeTerminalTitle(value);
    if (next === this.#title) return null;
    this.#title = next;
    return this.#advance();
  }

  setCwd(value: string | null): TerminalMetadataSnapshot | null {
    if (value === this.#cwd) return null;
    this.#cwd = value;
    return this.#advance();
  }

  current(): TerminalMetadataSnapshot | null {
    return this.#revision === 0 ? null : this.#snapshot();
  }

  rebase(revision: number): void {
    if (Number.isSafeInteger(revision) && revision >= 0) {
      this.#revision = Math.max(this.#revision, revision);
    }
  }

  #advance(): TerminalMetadataSnapshot {
    this.#revision += 1;
    return this.#snapshot();
  }

  #snapshot(): TerminalMetadataSnapshot {
    return { revision: this.#revision, title: this.#title, cwd: this.#cwd };
  }
}

export function normalizeTerminalTitle(value: string): string | null {
  const normalized = Array.from(
    value,
    (character) => isControlCharacter(character) ? " " : character,
  ).join("").replace(/\s+/gu, " ")
    .trim();
  if (normalized.length === 0) return null;
  return truncateUtf8(normalized, MAX_TERMINAL_TITLE_BYTES);
}

export function parseTerminalCwdOsc(value: string): string | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (
    url.protocol !== "file:" ||
    (url.hostname !== "" && url.hostname !== "localhost")
  ) {
    return null;
  }
  let path: string;
  try {
    path = decodeURIComponent(url.pathname);
  } catch {
    return null;
  }
  if (!path.startsWith("/") || Array.from(path).some(isControlCharacter)) {
    return null;
  }
  const bounded = truncateUtf8(path, MAX_TERMINAL_CWD_BYTES);
  return bounded.startsWith("/") ? bounded : null;
}

function isControlCharacter(character: string): boolean {
  const codePoint = character.codePointAt(0) ?? 0;
  return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
}

function truncateUtf8(value: string, maximum: number): string {
  if (encoder.encode(value).byteLength <= maximum) return value;
  let result = "";
  for (const character of value) {
    if (encoder.encode(result + character).byteLength > maximum) break;
    result += character;
  }
  return result;
}
