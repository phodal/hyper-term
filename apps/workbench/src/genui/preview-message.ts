import {
  MAX_RUNTIME_ERROR_MESSAGE_BYTES,
  MAX_RUNTIME_ERROR_STACK_BYTES,
} from "../../genui/runtime-error.ts";
import {
  isRuntimeTraceMessage,
  type RuntimeTraceInput,
} from "../runtime-trace-client.ts";
import type { VisualCaptureObservation } from "../../genui/visual-quality-measure.ts";

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

export interface PreviewQualityCaptureMessage extends PreviewMessageBase {
  type: "hyper_term_preview_quality_capture";
  artifact_id: string;
  source_revision: number;
  artifact_digest: string;
  observation: VisualCaptureObservation;
}

export type PreviewMessage =
  | PreviewBootMessage
  | PreviewReadyMessage
  | PreviewErrorMessage
  | PreviewTraceMessage
  | PreviewQualityCaptureMessage;

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
  if (
    value.type === "hyper_term_preview_quality_capture" &&
    sha256(value.artifact_digest) && validVisualObservation(value.observation)
  ) return value as unknown as PreviewQualityCaptureMessage;
  return undefined;
}

function validVisualObservation(value: unknown): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const capture = value as Record<string, unknown>;
  const viewport = capture.viewport as Record<string, unknown> | undefined;
  const samples = capture.samples;
  const boundedCounts = [
    capture.document_width,
    capture.document_height,
    capture.element_count,
    capture.interactive_count,
    capture.clipped_count,
    capture.undersized_target_count,
    capture.low_contrast_count,
    capture.hidden_primary_action_count,
    capture.focus_target_count,
    capture.focus_visible_count,
    capture.content_fixture_target_count,
    capture.content_fixture_applied_count,
    capture.content_fixture_cjk_label_count,
    capture.content_fixture_long_content_count,
    capture.console_error_count,
    capture.resource_failure_count,
    capture.layout_shift_milli,
  ];
  return boundedString(capture.capture_id, 64) &&
    viewport !== undefined && positiveSafeInteger(viewport.width) &&
    positiveSafeInteger(viewport.height) &&
    (capture.color_scheme === "light" || capture.color_scheme === "dark") &&
    (capture.locale === "en" || capture.locale === "zh-CN") &&
    (capture.scenario === "default" || capture.scenario === "focus-first" ||
      capture.scenario === "content-stress") &&
    typeof capture.reduced_motion === "boolean" &&
    boundedCounts.every((count) =>
      nonNegativeBoundedInteger(count, 1_000_000)
    ) && validContentFixtureObservation(capture) &&
    sha256(capture.semantic_digest) && Array.isArray(samples) &&
    samples.length <= 24 && samples.every(validVisualSample);
}

function validContentFixtureObservation(
  capture: Record<string, unknown>,
): boolean {
  const targets = Number(capture.content_fixture_target_count);
  const applied = Number(capture.content_fixture_applied_count);
  const cjk = Number(capture.content_fixture_cjk_label_count);
  const longContent = Number(capture.content_fixture_long_content_count);
  if (
    targets > 2 || applied > targets || cjk > applied ||
    longContent > applied
  ) return false;
  if (capture.scenario === "content-stress") {
    return capture.locale === "zh-CN" && sha256(capture.content_fixture_digest);
  }
  return capture.content_fixture_digest === undefined && targets === 0 &&
    applied === 0 && cjk === 0 && longContent === 0;
}

function validVisualSample(value: unknown): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const sample = value as Record<string, unknown>;
  const categories = new Set([
    "empty_render",
    "viewport_overflow",
    "clipped_content",
    "undersized_target",
    "low_contrast",
    "hidden_primary_action",
    "missing_focus_indicator",
    "console_error",
    "resource_failure",
    "layout_instability",
  ]);
  if (
    typeof sample.category !== "string" || !categories.has(sample.category) ||
    !boundedString(sample.semantic_path, 256)
  ) return false;
  if (sample.rect === undefined) return true;
  if (
    !sample.rect || typeof sample.rect !== "object" ||
    Array.isArray(sample.rect)
  ) {
    return false;
  }
  const rect = sample.rect as Record<string, unknown>;
  return Number.isSafeInteger(rect.x) && Number.isSafeInteger(rect.y) &&
    nonNegativeBoundedInteger(rect.width, 1_000_000) &&
    nonNegativeBoundedInteger(rect.height, 1_000_000);
}

function sha256(value: unknown): boolean {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}

function nonNegativeBoundedInteger(value: unknown, maximum: number): boolean {
  return Number.isSafeInteger(value) && Number(value) >= 0 &&
    Number(value) <= maximum;
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
