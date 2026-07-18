import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  decodeTerminalBinary,
  encodeTerminalInput,
  MAX_TERMINAL_WEB_PAYLOAD_BYTES,
  TERMINAL_WEB_PROTOCOL_VERSION,
  TerminalWebBinaryKind,
  type TerminalWebClientControl,
  type TerminalWebServerControl,
} from "./protocol.ts";
import {
  terminalAttachmentStorageKey,
  TerminalConnectionState,
  terminalSessionId,
} from "./connection-state.ts";
import "./styles.css";

const attachmentStorageKey = terminalAttachmentStorageKey(
  globalThis.location.href,
);
const sessionId = terminalSessionId(globalThis.location.href);
const terminalElement = requiredElement("#terminal");
const statusElement = requiredElement("#connection-status");

const terminal = new Terminal({
  allowProposedApi: false,
  allowTransparency: false,
  cursorBlink: true,
  cursorStyle: "block",
  convertEol: false,
  disableStdin: false,
  drawBoldTextInBrightColors: true,
  fontFamily:
    "SFMono-Regular, ui-monospace, Menlo, Monaco, Consolas, monospace",
  fontSize: 13,
  fontWeight: "400",
  fontWeightBold: "600",
  letterSpacing: 0,
  lineHeight: 1.2,
  macOptionIsMeta: true,
  minimumContrastRatio: 4.5,
  rightClickSelectsWord: true,
  scrollback: 20_000,
  smoothScrollDuration: 0,
  theme: {
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
  },
});
const fit = new FitAddon();
terminal.loadAddon(fit);
terminal.open(terminalElement);
fitTerminal();
terminal.focus();

let socket: WebSocket | null = null;
let reconnectTimer: number | null = null;
let reconnectAttempt = 0;
let afterSequence = 0n;
let attachmentId = readAttachmentId();
const connectionState = new TerminalConnectionState();

terminal.onData((data) => sendInput(new TextEncoder().encode(data)));
terminal.onBinary((data) => {
  const bytes = Uint8Array.from(data, (character) => character.charCodeAt(0));
  sendInput(bytes);
});

const resizeObserver = new ResizeObserver(() => {
  fitTerminal();
  if (!connectionState.canSend(socket?.readyState === WebSocket.OPEN)) return;
  sendControl({
    type: "resize",
    generation: connectionState.takeResizeGeneration(),
    size: terminalSize(),
  });
});
resizeObserver.observe(terminalElement);
globalThis.addEventListener("focus", () => terminal.focus());
globalThis.addEventListener("online", connect);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) terminal.focus();
});

connect();

function connect(): void {
  if (sessionId === null) {
    setStatus("Invalid terminal session", true);
    return;
  }
  if (
    socket?.readyState === WebSocket.OPEN ||
    socket?.readyState === WebSocket.CONNECTING
  ) return;
  if (reconnectTimer !== null) {
    globalThis.clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  connectionState.beginConnection();
  setStatus(reconnectAttempt === 0 ? "Connecting…" : "Reconnecting…", true);
  const url = new URL("/terminal", globalThis.location.href);
  url.protocol = globalThis.location.protocol === "https:" ? "wss:" : "ws:";
  const token = new URL(globalThis.location.href).searchParams.get("token");
  if (token) url.searchParams.set("token", token);

  socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  socket.addEventListener("open", () => {
    reconnectAttempt = 0;
    sendControl({
      type: "hello",
      protocol_version: TERMINAL_WEB_PROTOCOL_VERSION,
      session_id: sessionId,
      attachment_id: attachmentId,
      after_sequence: safeNumber(afterSequence),
      size: terminalSize(),
      cwd: null,
    });
    fitTerminal();
  });
  socket.addEventListener("message", (event) => receive(event.data));
  socket.addEventListener("close", () => {
    connectionState.disconnect();
    socket = null;
    scheduleReconnect();
  });
  socket.addEventListener("error", () => socket?.close());
}

function receive(data: string | ArrayBuffer | Blob): void {
  if (typeof data === "string") {
    receiveControl(JSON.parse(data) as TerminalWebServerControl);
    return;
  }
  if (data instanceof Blob) {
    void data.arrayBuffer().then(receiveBinary).catch(showProtocolError);
    return;
  }
  receiveBinary(data);
}

function receiveControl(message: TerminalWebServerControl): void {
  switch (message.type) {
    case "ready":
      if (message.protocol_version !== TERMINAL_WEB_PROTOCOL_VERSION) {
        showProtocolError(
          new Error(
            `server protocol ${message.protocol_version} is unsupported`,
          ),
        );
        socket?.close();
        return;
      }
      attachmentId = message.attachment_id;
      writeAttachmentId(attachmentId);
      connectionState.acceptReady(
        message.next_input_sequence,
        message.resize_generation,
      );
      setStatus("Connected", false);
      terminal.focus();
      break;
    case "exited":
      setStatus(
        message.signal
          ? `Shell exited (${message.signal})`
          : `Shell exited (${message.exit_code ?? "unknown"})`,
        true,
      );
      break;
    case "error":
      setStatus(message.message, true);
      if (message.code === "unauthorized" || message.code === "protocol") {
        socket?.close(1008, message.code);
      }
      break;
  }
}

function receiveBinary(encoded: ArrayBuffer): void {
  try {
    const frame = decodeTerminalBinary(encoded);
    if (frame.kind === TerminalWebBinaryKind.Input) {
      throw new Error("server sent a terminal input frame");
    }
    if (frame.kind === TerminalWebBinaryKind.Output) {
      if (frame.sequence <= afterSequence) return;
      afterSequence = frame.sequence;
      terminal.write(frame.bytes);
      return;
    }
    terminal.reset();
    terminal.write(frame.bytes);
    afterSequence = frame.nextSequence > 0n ? frame.nextSequence - 1n : 0n;
  } catch (error) {
    showProtocolError(error);
    socket?.close(1002, "invalid terminal frame");
  }
}

function sendInput(bytes: Uint8Array): void {
  if (!connectionState.canSend(socket?.readyState === WebSocket.OPEN)) return;
  for (let offset = 0; offset < bytes.byteLength;) {
    const end = Math.min(
      bytes.byteLength,
      offset + MAX_TERMINAL_WEB_PAYLOAD_BYTES,
    );
    socket?.send(
      encodeTerminalInput(
        connectionState.takeInputSequence(),
        bytes.slice(offset, end),
      ),
    );
    offset = end;
  }
}

function sendControl(message: TerminalWebClientControl): void {
  socket?.send(JSON.stringify(message));
}

function terminalSize() {
  const bounds = terminalElement.getBoundingClientRect();
  return {
    cols: Math.max(1, Math.min(1_000, terminal.cols)),
    rows: Math.max(1, Math.min(1_000, terminal.rows)),
    pixel_width: Math.min(65_535, Math.round(bounds.width)),
    pixel_height: Math.min(65_535, Math.round(bounds.height)),
  };
}

function scheduleReconnect(): void {
  if (reconnectTimer !== null) return;
  reconnectAttempt += 1;
  const delay = Math.min(4_000, 150 * 2 ** Math.min(reconnectAttempt, 5));
  setStatus(
    `Disconnected · retrying in ${Math.round(delay / 100) / 10}s`,
    true,
  );
  reconnectTimer = globalThis.setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, delay);
}

function setStatus(message: string, visible: boolean): void {
  statusElement.textContent = message;
  statusElement.toggleAttribute("data-visible", visible);
}

function fitTerminal(): void {
  try {
    fit.fit();
  } catch {
    setStatus("Waiting for terminal layout…", true);
  }
}

function showProtocolError(error: unknown): void {
  setStatus(error instanceof Error ? error.message : String(error), true);
}

function readAttachmentId(): string | null {
  try {
    return globalThis.localStorage.getItem(attachmentStorageKey);
  } catch {
    return null;
  }
}

function writeAttachmentId(value: string): void {
  try {
    globalThis.localStorage.setItem(attachmentStorageKey, value);
  } catch {
    // Reconnect remains available for the lifetime of this document.
  }
}

function safeNumber(value: bigint): number {
  return Number(value > BigInt(Number.MAX_SAFE_INTEGER) ? 0n : value);
}

function requiredElement(selector: string): HTMLElement {
  const element = document.querySelector<HTMLElement>(selector);
  if (!element) throw new Error(`terminal document is missing ${selector}`);
  return element;
}
