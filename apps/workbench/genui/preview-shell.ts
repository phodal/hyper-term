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
import type { RuntimeTraceEvent } from "../src/runtime-trace-client.ts";
import {
  runReplayableEffect,
  RuntimeReplaySession,
  verifyReplayProjectionDigest,
} from "../src/genui/runtime-replay.ts";
import {
  measureVisualQuality,
  type VisualQualityMeasureRequest,
  type VisualQualityRuntimeCounters,
} from "./visual-quality-measure.ts";
import {
  applyPreviewQualityEnvironment,
  previewQualityEnvironment,
  rewritePreferenceMediaStyle,
} from "./preview-environment.ts";

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

interface ReplayArtifactMessage extends Omit<RenderArtifactMessage, "type"> {
  type: "hyper_term_replay_artifact";
  replay: {
    source_revision: number;
    target_event_sequence: number;
    projection_digest: string;
    events: RuntimeTraceEvent[];
  };
}

interface MeasureVisualQualityMessage {
  type: "hyper_term_measure_visual_quality";
  schema_version: 1;
  channel_token: string;
  artifact_id: string;
  source_revision: number;
  capture: VisualQualityMeasureRequest;
}

type RuntimeTraceKind =
  | "action"
  | "checkpoint"
  | "effect_receipt"
  | "console"
  | "error";

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
  var __HYPER_USE_REPLAY_REDUCER__: <State, Action>(
    name: string,
    reducer: (state: State, action: Action) => State,
    initialState: State,
  ) => [State, React.Dispatch<Action>];
  var __HYPER_EFFECT__: <T>(
    name: string,
    input: unknown,
    invoke: () => T | Promise<T>,
  ) => Promise<T>;
  var __HYPER_BOOTSTRAP_ARTIFACT__:
    | RenderArtifactMessage["artifact"]
    | undefined;
}

const MAX_ARTIFACT_BYTES = 2 * 1024 * 1024;
const MAX_RUNTIME_TRACE_BYTES = 32 * 1024;
const MAX_RUNTIME_TRACE_NAME_BYTES = 128;
const channelToken = location.hash.slice(1);
const qualityEnvironment = previewQualityEnvironment(new URL(location.href));
if (qualityEnvironment) applyPreviewQualityEnvironment(qualityEnvironment);
let runtimeTraceStreamId = crypto.randomUUID();
let runtimeTraceSequence = 0;
let replaySession: RuntimeReplaySession | undefined;
const rootElement = requiredElement("root");
const runtimeErrorElement = requiredElement("runtime-error");
let root: Root | undefined;
let currentModule: string | undefined;
let activeArtifact:
  | Pick<
    RenderArtifactMessage["artifact"],
    "artifact_id" | "source_revision" | "content_digest" | "source_map"
  >
  | undefined;
const visualQualityCounters: VisualQualityRuntimeCounters = {
  consoleErrors: 0,
  resourceFailures: 0,
  layoutShiftMilli: 0,
};

const originalConsoleError = console.error.bind(console);
console.error = (...values: unknown[]) => {
  visualQualityCounters.consoleErrors++;
  originalConsoleError(...values);
};
globalThis.addEventListener("error", (event) => {
  if (event.target instanceof HTMLElement && event.target !== document.body) {
    visualQualityCounters.resourceFailures++;
  }
}, true);
if (
  typeof PerformanceObserver !== "undefined" &&
  PerformanceObserver.supportedEntryTypes.includes("layout-shift")
) {
  const observer = new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) {
      const shift = entry as PerformanceEntry & {
        value?: number;
        hadRecentInput?: boolean;
      };
      if (!shift.hadRecentInput && typeof shift.value === "number") {
        visualQualityCounters.layoutShiftMilli = Math.min(
          10_000,
          visualQualityCounters.layoutShiftMilli +
            Math.round(shift.value * 1_000),
        );
      }
    }
  });
  observer.observe({ type: "layout-shift", buffered: true });
}

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
    replaySession || !activeArtifact || !validRuntimeTraceKind(kind) ||
    !validTraceName(name)
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
globalThis.__HYPER_USE_REPLAY_REDUCER__ = <State, Action>(
  name: string,
  reducer: (state: State, action: Action) => State,
  initialState: State,
): [State, React.Dispatch<Action>] => {
  if (!validTraceName(name)) {
    throw new Error("Replay reducer name is invalid.");
  }
  if (replaySession) {
    const [state] = React.useState(() =>
      replaySession?.reduce(name, initialState, reducer) ?? initialState
    );
    return [state, () => {}];
  }
  const [state, dispatch] = React.useReducer(reducer, initialState);
  const tracedDispatch = React.useCallback((action: Action) => {
    globalThis.__HYPER_TRACE__("action", name, { action });
    dispatch(action);
  }, [name]);
  return [state, tracedDispatch];
};
globalThis.__HYPER_EFFECT__ = async <T>(
  name: string,
  input: unknown,
  invoke: () => T | Promise<T>,
): Promise<T> => {
  if (!validTraceName(name) || typeof invoke !== "function") {
    throw new Error("Replayable effect contract is invalid.");
  }
  return await runReplayableEffect(
    replaySession,
    name,
    input,
    invoke,
    (payload) => globalThis.__HYPER_TRACE__("effect_receipt", name, payload),
  );
};

globalThis.addEventListener("message", (event: MessageEvent<unknown>) => {
  if (isRenderMessage(event.data)) {
    void render(event.data);
  } else if (isVisualQualityMeasureMessage(event.data)) {
    void captureVisualQuality(event.data);
  }
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

async function render(
  message: RenderArtifactMessage | ReplayArtifactMessage,
): Promise<void> {
  const { artifact } = message;
  clearRuntimeError();
  if (message.type === "hyper_term_replay_artifact") {
    if (
      !await verifyReplayProjectionDigest(
        message.replay.source_revision,
        message.replay.events,
        message.replay.projection_digest,
      ) || !message.replay.events.every((event) =>
        event.source_revision === message.replay.source_revision
      )
    ) {
      reportRuntimeError("runtime replay projection digest mismatch", {
        artifact_id: artifact.artifact_id,
        source_revision: artifact.source_revision,
        content_digest: artifact.content_digest,
        source_map: artifact.source_map,
      });
      return;
    }
    replaySession = new RuntimeReplaySession(
      message.replay.events,
      message.replay.target_event_sequence,
      message.replay.projection_digest,
    );
  } else {
    replaySession = undefined;
  }
  // Each accepted render is a distinct semantic stream. This prevents a
  // discarded local draft from creating sequence gaps when the editor returns
  // to the Rust-accepted source revision.
  runtimeTraceStreamId = crypto.randomUUID();
  runtimeTraceSequence = 0;
  visualQualityCounters.consoleErrors = 0;
  visualQualityCounters.resourceFailures = 0;
  visualQualityCounters.layoutShiftMilli = 0;
  activeArtifact = {
    artifact_id: artifact.artifact_id,
    source_revision: artifact.source_revision,
    content_digest: artifact.content_digest,
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
  const existingStyle = document.getElementById("artifact-style");
  if (existingStyle && !(existingStyle instanceof HTMLStyleElement)) {
    throw new Error("artifact style boundary changed type");
  }
  const style = existingStyle ?? document.head.appendChild(
    Object.assign(document.createElement("style"), {
      id: "artifact-style",
    }),
  );
  style.textContent = artifact.css;
  if (qualityEnvironment) {
    rewritePreferenceMediaStyle(style, qualityEnvironment);
  }
  if (currentModule) URL.revokeObjectURL(currentModule);
  currentModule = URL.createObjectURL(
    new Blob([artifact.bundle], { type: "text/javascript" }),
  );
  try {
    if (root) {
      root.unmount();
      root = undefined;
    }
    await import(currentModule);
    report("hyper_term_preview_ready", {
      artifact_id: artifact.artifact_id,
      source_revision: artifact.source_revision,
      ...(replaySession
        ? {
          replay: true,
          target_event_sequence: replaySession.targetEventSequence,
          projection_digest: replaySession.projectionDigest,
        }
        : {}),
    });
  } catch (error) {
    reportRuntimeError(error, activeArtifact);
  }
}

async function captureVisualQuality(
  message: MeasureVisualQualityMessage,
): Promise<void> {
  if (
    !activeArtifact || message.artifact_id !== activeArtifact.artifact_id ||
    message.source_revision !== activeArtifact.source_revision
  ) return;
  try {
    if (
      !qualityEnvironment ||
      message.capture.color_scheme !== qualityEnvironment.colorScheme ||
      message.capture.reduced_motion !== qualityEnvironment.reducedMotion
    ) {
      throw new Error("visual quality environment changed before capture");
    }
    const observation = await measureVisualQuality(
      rootElement,
      message.capture,
      visualQualityCounters,
    );
    report("hyper_term_preview_quality_capture", {
      artifact_id: activeArtifact.artifact_id,
      source_revision: activeArtifact.source_revision,
      artifact_digest: activeArtifact.content_digest,
      observation,
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

function isRenderMessage(
  value: unknown,
): value is RenderArtifactMessage | ReplayArtifactMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as {
    type?: string;
    schema_version?: number;
    channel_token?: string;
    artifact?: Partial<RenderArtifactMessage["artifact"]>;
    replay?: Partial<ReplayArtifactMessage["replay"]>;
  };
  return (message.type === "hyper_term_render_artifact" ||
    message.type === "hyper_term_replay_artifact") &&
    message.schema_version === 1 &&
    message.channel_token === channelToken &&
    typeof message.artifact?.artifact_id === "string" &&
    typeof message.artifact.bundle === "string" &&
    typeof message.artifact.css === "string" &&
    typeof message.artifact.source_map === "string" &&
    (message.type !== "hyper_term_replay_artifact" ||
      (Number.isSafeInteger(message.replay?.target_event_sequence) &&
        Number(message.replay?.target_event_sequence) > 0 &&
        Number.isSafeInteger(message.replay?.source_revision) &&
        Number(message.replay?.source_revision) > 0 &&
        typeof message.replay?.projection_digest === "string" &&
        Array.isArray(message.replay?.events)));
}

function isVisualQualityMeasureMessage(
  value: unknown,
): value is MeasureVisualQualityMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as Partial<MeasureVisualQualityMessage>;
  return message.type === "hyper_term_measure_visual_quality" &&
    message.schema_version === 1 && message.channel_token === channelToken &&
    typeof message.artifact_id === "string" &&
    Number.isSafeInteger(message.source_revision) &&
    message.source_revision! > 0 &&
    Boolean(message.capture) &&
    typeof message.capture?.capture_id === "string" &&
    Number.isSafeInteger(message.capture.viewport?.width) &&
    message.capture.viewport.width > 0 &&
    message.capture.viewport.width <= 4_096 &&
    Number.isSafeInteger(message.capture.viewport?.height) &&
    message.capture.viewport.height > 0 &&
    message.capture.viewport.height <= 4_096 &&
    (message.capture.color_scheme === "light" ||
      message.capture.color_scheme === "dark") &&
    message.capture.locale === "en" &&
    (message.capture.scenario === "default" ||
      message.capture.scenario === "focus-first") &&
    typeof message.capture.reduced_motion === "boolean";
}

function validRuntimeTraceKind(value: string): value is RuntimeTraceKind {
  return value === "action" || value === "checkpoint" ||
    value === "effect_receipt" || value === "console" || value === "error";
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
