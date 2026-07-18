import React from "react";
import { createRoot, type Root } from "react-dom/client";
import * as JsxDevRuntime from "react/jsx-dev-runtime";
import * as JsxRuntime from "react/jsx-runtime";

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
  };
}

declare global {
  var __HYPER_REACT__: typeof React;
  var __HYPER_JSX_RUNTIME__: typeof JsxRuntime;
  var __HYPER_JSX_DEV_RUNTIME__: typeof JsxDevRuntime;
  var __HYPER_MOUNT__: (component: React.ComponentType) => void;
  var __HYPER_BOOTSTRAP_ARTIFACT__:
    | RenderArtifactMessage["artifact"]
    | undefined;
}

const MAX_ARTIFACT_BYTES = 2 * 1024 * 1024;
const channelToken = location.hash.slice(1);
const rootElement = document.getElementById("root");
if (!rootElement) throw new Error("isolated preview is missing #root");
let root: Root | undefined;
let currentModule: string | undefined;

globalThis.__HYPER_REACT__ = React;
globalThis.__HYPER_JSX_RUNTIME__ = JsxRuntime;
globalThis.__HYPER_JSX_DEV_RUNTIME__ = JsxDevRuntime;
globalThis.__HYPER_MOUNT__ = (component) => {
  root ??= createRoot(rootElement);
  root.render(React.createElement(component));
};

globalThis.addEventListener("message", (event: MessageEvent<unknown>) => {
  if (!isRenderMessage(event.data)) {
    return;
  }
  void render(event.data);
});

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
  if (
    new TextEncoder().encode(artifact.bundle + artifact.css).byteLength >
      MAX_ARTIFACT_BYTES
  ) {
    report("hyper_term_preview_error", {
      artifact_id: artifact.artifact_id,
      message: "accepted artifact exceeds preview bound",
    });
    return;
  }
  if (
    await sha256(artifact.bundle + artifact.css) !== artifact.content_digest
  ) {
    report("hyper_term_preview_error", {
      artifact_id: artifact.artifact_id,
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
    report("hyper_term_preview_error", {
      artifact_id: artifact.artifact_id,
      message: error instanceof Error ? error.message : String(error),
    });
  }
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
    typeof message.artifact.css === "string";
}

function report(type: string, detail: Record<string, unknown> = {}): void {
  globalThis.parent.postMessage(
    { type, schema_version: 1, channel_token: channelToken, ...detail },
    "*",
  );
}
