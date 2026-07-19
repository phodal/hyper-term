import React from "react";
import { createRoot, type Root } from "react-dom/client";
import * as JsxDevRuntime from "react/jsx-dev-runtime";
import * as JsxRuntime from "react/jsx-runtime";
import {
  boundedRuntimeText,
  generatedPositionFromStack,
  MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  MAX_RUNTIME_ERROR_STACK_BYTES,
} from "./runtime-error.ts";
import {
  mapPreviewRuntimeError,
  MAX_RUNTIME_SOURCE_MAP_BYTES,
} from "../src/genui/runtime-diagnostic.ts";

interface RenderArtifactMessage {
  type: "hyper_term_render_artifact";
  schema_version: 1;
  channel_token: string;
  artifact: {
    artifact_id: string;
    source_revision: number;
    content_digest: string;
    bundle: string;
    css: string;
    source_map: string;
  };
}

type RuntimeTraceKind = "action" | "checkpoint" | "console" | "error";

declare global {
  var __HYPER_REACT__: typeof React;
  var __HYPER_JSX_RUNTIME__: typeof JsxRuntime;
  var __HYPER_JSX_DEV_RUNTIME__: typeof JsxDevRuntime;
  var __HYPER_MOUNT__: (component: React.ComponentType) => void;
  var __HYPER_TRACE__: (
    kind: RuntimeTraceKind,
    name: string,
    payload?: unknown,
  ) => void;
  var __HYPER_BOOTSTRAP_ARTIFACT__:
    | RenderArtifactMessage["artifact"]
    | undefined;
}

const MAX_ARTIFACT_BYTES = 2 * 1024 * 1024;
const MAX_RUNTIME_TRACE_BYTES = 32 * 1024;
const MAX_RUNTIME_TRACE_NAME_BYTES = 128;
const channelToken = location.hash.slice(1);
let runtimeTraceStreamId = crypto.randomUUID();
let runtimeTraceSequence = 0;
const rootElement = requiredElement("root");
const runtimeErrorElement = requiredElement("runtime-error");
let root: Root | undefined;
let currentModule: string | undefined;
let activeArtifact:
  | Pick<
    RenderArtifactMessage["artifact"],
    "artifact_id" | "source_revision" | "source_map"
  >
  | undefined;

function requiredElement(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (!element) throw new Error(`isolated preview is missing #${id}`);
  return element;
}

globalThis.__HYPER_REACT__ = React;
globalThis.__HYPER_JSX_RUNTIME__ = JsxRuntime;
globalThis.__HYPER_JSX_DEV_RUNTIME__ = JsxDevRuntime;
globalThis.__HYPER_MOUNT__ = (component) => {
  root ??= createRoot(rootElement);
  root.render(React.createElement(component));
};
globalThis.__HYPER_TRACE__ = (kind, name, payload = null) => {
  if (
    !activeArtifact || !validRuntimeTraceKind(kind) || !validTraceName(name)
  ) {
    return;
  }
  let cloned: unknown;
  try {
    const encoded = JSON.stringify(payload);
    if (
      encoded === undefined ||
      new TextEncoder().encode(encoded).byteLength > MAX_RUNTIME_TRACE_BYTES
    ) return;
    cloned = JSON.parse(encoded);
  } catch {
    return;
  }
  const event = {
    schema_version: 1 as const,
    stream_id: runtimeTraceStreamId,
    client_sequence: runtimeTraceSequence + 1,
    kind,
    name,
    payload: cloned,
  };
  if (
    new TextEncoder().encode(JSON.stringify(event)).byteLength >
      MAX_RUNTIME_TRACE_BYTES
  ) return;
  runtimeTraceSequence = event.client_sequence;
  report("hyper_term_preview_trace", {
    artifact_id: activeArtifact.artifact_id,
    source_revision: activeArtifact.source_revision,
    event,
  });
};

globalThis.addEventListener("message", (event: MessageEvent<unknown>) => {
  if (!isRenderMessage(event.data)) {
    return;
  }
  void render(event.data);
});
globalThis.addEventListener("error", (event: ErrorEvent) => {
  reportRuntimeError(event.error ?? event.message);
});
globalThis.addEventListener(
  "unhandledrejection",
  (event: PromiseRejectionEvent) => {
    reportRuntimeError(event.reason);
  },
);

report("hyper_term_preview_boot");
if (globalThis.__HYPER_BOOTSTRAP_ARTIFACT__) {
  const artifact = globalThis.__HYPER_BOOTSTRAP_ARTIFACT__;
  globalThis.__HYPER_BOOTSTRAP_ARTIFACT__ = undefined;
  void render({
    type: "hyper_term_render_artifact",
    schema_version: 1,
    channel_token: artifact.artifact_id,
    artifact,
  });
}

async function render(message: RenderArtifactMessage): Promise<void> {
  const { artifact } = message;
  clearRuntimeError();
  // Each accepted render is a distinct semantic stream. This prevents a
  // discarded local draft from creating sequence gaps when the editor returns
  // to the Rust-accepted source revision.
  runtimeTraceStreamId = crypto.randomUUID();
  runtimeTraceSequence = 0;
  activeArtifact = {
    artifact_id: artifact.artifact_id,
    source_revision: artifact.source_revision,
    source_map: artifact.source_map,
  };
  if (
    new TextEncoder().encode(artifact.bundle + artifact.css).byteLength >
      MAX_ARTIFACT_BYTES ||
    new TextEncoder().encode(artifact.source_map).byteLength >
      MAX_RUNTIME_SOURCE_MAP_BYTES
  ) {
    showRuntimeError("accepted artifact exceeds preview bound");
    report("hyper_term_preview_error", {
      artifact_id: artifact.artifact_id,
      source_revision: artifact.source_revision,
      message: "accepted artifact exceeds preview bound",
    });
    return;
  }
  if (
    await sha256(artifact.bundle + artifact.css) !== artifact.content_digest
  ) {
    showRuntimeError("accepted artifact digest changed in transit");
    report("hyper_term_preview_error", {
      artifact_id: artifact.artifact_id,
      source_revision: artifact.source_revision,
      message: "accepted artifact digest changed in transit",
    });
    return;
  }
  const style = document.getElementById("artifact-style") ??
    document.head.appendChild(Object.assign(document.createElement("style"), {
      id: "artifact-style",
    }));
  style.textContent = artifact.css;
  if (currentModule) URL.revokeObjectURL(currentModule);
  currentModule = URL.createObjectURL(
    new Blob([artifact.bundle], { type: "text/javascript" }),
  );
  try {
    await import(currentModule);
    report("hyper_term_preview_ready", {
      artifact_id: artifact.artifact_id,
      source_revision: artifact.source_revision,
    });
  } catch (error) {
    reportRuntimeError(error, activeArtifact);
  }
}

function reportRuntimeError(
  value: unknown,
  artifact = activeArtifact,
): void {
  if (!artifact) return;
  const message = boundedRuntimeText(
    value instanceof Error ? value.message : String(value),
    MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  );
  const stackValue = value && typeof value === "object" && "stack" in value
    ? (value as { stack?: unknown }).stack
    : undefined;
  const stack = typeof stackValue === "string"
    ? boundedRuntimeText(stackValue, MAX_RUNTIME_ERROR_STACK_BYTES)
    : undefined;
  const generated = stack ? generatedPositionFromStack(stack) : undefined;
  const diagnostic = mapPreviewRuntimeError({
    message,
    stack,
    generated_line: generated?.line,
    generated_column: generated?.column,
  }, artifact.source_map);
  showRuntimeError(
    diagnostic.message,
    diagnostic.original ?? diagnostic.generated,
  );
  report("hyper_term_preview_error", {
    artifact_id: artifact.artifact_id,
    source_revision: artifact.source_revision,
    message: diagnostic.message,
    ...(diagnostic.stack ? { stack: diagnostic.stack } : {}),
    ...(generated
      ? {
        generated_line: generated.line,
        generated_column: generated.column,
      }
      : {}),
    ...(diagnostic.original
      ? {
        original_file: diagnostic.original.file,
        original_line: diagnostic.original.line,
        original_column: diagnostic.original.column,
      }
      : {}),
  });
}

async function sha256(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function isRenderMessage(value: unknown): value is RenderArtifactMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as Partial<RenderArtifactMessage>;
  return message.type === "hyper_term_render_artifact" &&
    message.schema_version === 1 &&
    message.channel_token === channelToken &&
    typeof message.artifact?.artifact_id === "string" &&
    typeof message.artifact.bundle === "string" &&
    typeof message.artifact.css === "string" &&
    typeof message.artifact.source_map === "string";
}

function validRuntimeTraceKind(value: string): value is RuntimeTraceKind {
  return value === "action" || value === "checkpoint" || value === "console" ||
    value === "error";
}

function validTraceName(value: string): boolean {
  return value.length > 0 &&
    new TextEncoder().encode(value).byteLength <=
      MAX_RUNTIME_TRACE_NAME_BYTES &&
    !value.includes("\0") && !value.includes("\n") && !value.includes("\r");
}

function report(type: string, detail: Record<string, unknown> = {}): void {
  globalThis.parent.postMessage(
    { type, schema_version: 1, channel_token: channelToken, ...detail },
    "*",
  );
}

function clearRuntimeError(): void {
  runtimeErrorElement.hidden = true;
  runtimeErrorElement.textContent = "";
}

function showRuntimeError(
  message: string,
  location?: { file?: string; line: number; column: number },
): void {
  const locationText = location
    ? `${
      location.file ?? "generated bundle"
    }:${location.line}:${location.column}`
    : "location unavailable";
  runtimeErrorElement.textContent =
    `Runtime error\n${locationText}\n${message}`;
  runtimeErrorElement.hidden = false;
}
