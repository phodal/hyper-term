import {
  MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  MAX_RUNTIME_ERROR_STACK_BYTES,
} from "../../genui/runtime-error.ts";
import {
  isRuntimeTraceMessage,
  type RuntimeTraceInput,
} from "../runtime-trace-client.ts";

export const MAX_PREVIEW_MESSAGE_BYTES = 64 * 1024;
const MAX_ARTIFACT_ID_BYTES = 128;

interface PreviewMessageBase {
  schema_version: 1;
  channel_token: string;
}

export interface PreviewBootMessage extends PreviewMessageBase {
  type: "hyper_term_preview_boot";
}

export interface PreviewReadyMessage extends PreviewMessageBase {
  type: "hyper_term_preview_ready";
  artifact_id: string;
  source_revision: number;
  replay?: boolean;
  target_event_sequence?: number;
}

export interface PreviewErrorMessage extends PreviewMessageBase {
  type: "hyper_term_preview_error";
  artifact_id: string;
  source_revision: number;
  message: string;
  stack?: string;
  generated_line?: number;
  generated_column?: number;
}

export interface PreviewTraceMessage extends PreviewMessageBase {
  type: "hyper_term_preview_trace";
  artifact_id: string;
  source_revision: number;
  event: RuntimeTraceInput;
}

export type PreviewMessage =
  | PreviewBootMessage
  | PreviewReadyMessage
  | PreviewErrorMessage
  | PreviewTraceMessage;

export function parsePreviewMessage(
  value: unknown,
  channelToken: string,
): PreviewMessage | undefined {
  if (!boundedJsonRecord(value)) return undefined;
  if (
    value.schema_version !== 1 || value.channel_token !== channelToken ||
    typeof value.type !== "string"
  ) return undefined;

  if (value.type === "hyper_term_preview_boot") {
    return {
      type: value.type,
      schema_version: 1,
      channel_token: channelToken,
    };
  }
  if (!validArtifactContext(value)) return undefined;

  if (value.type === "hyper_term_preview_ready") {
    if (
      value.replay !== undefined && typeof value.replay !== "boolean" ||
      value.target_event_sequence !== undefined &&
        !positiveSafeInteger(value.target_event_sequence)
    ) return undefined;
    return value as unknown as PreviewReadyMessage;
  }
  if (value.type === "hyper_term_preview_error") {
    if (
      !boundedString(value.message, MAX_RUNTIME_ERROR_MESSAGE_BYTES) ||
      value.stack !== undefined &&
        !boundedString(value.stack, MAX_RUNTIME_ERROR_STACK_BYTES) ||
      value.generated_line !== undefined &&
        !positiveSafeInteger(value.generated_line) ||
      value.generated_column !== undefined &&
        !positiveSafeInteger(value.generated_column)
    ) return undefined;
    return value as unknown as PreviewErrorMessage;
  }
  if (
    value.type === "hyper_term_preview_trace" &&
    isRuntimeTraceMessage(value.event)
  ) return value as unknown as PreviewTraceMessage;
  return undefined;
}

function boundedJsonRecord(
  value: unknown,
): value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  try {
    const encoded = JSON.stringify(value);
    return typeof encoded === "string" &&
      new TextEncoder().encode(encoded).byteLength <= MAX_PREVIEW_MESSAGE_BYTES;
  } catch {
    return false;
  }
}

function validArtifactContext(value: Record<string, unknown>): boolean {
  return boundedString(value.artifact_id, MAX_ARTIFACT_ID_BYTES) &&
    positiveSafeInteger(value.source_revision);
}

function boundedString(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.length > 0 &&
    new TextEncoder().encode(value).byteLength <= maxBytes;
}

function positiveSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) > 0;
}
