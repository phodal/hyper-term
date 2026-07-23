import { assertEquals } from "@std/assert";
import {
  MAX_PREVIEW_MESSAGE_BYTES,
  parsePreviewMessage,
} from "./preview-message.ts";

const channel = "3ebaf681-975e-45ca-8c23-a5648b6611f9";
const artifact = "6570277e-e3ee-4bf8-a830-696d7abfeb8f";

Deno.test("preview messages accept only the bounded channel-bound vocabulary", () => {
  assertEquals(
    parsePreviewMessage({
      type: "hyper_term_preview_boot",
      schema_version: 1,
      channel_token: channel,
    }, channel)?.type,
    "hyper_term_preview_boot",
  );
  assertEquals(
    parsePreviewMessage({
      type: "hyper_term_preview_ready",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 3,
      replay: true,
      target_event_sequence: 9,
    }, channel)?.type,
    "hyper_term_preview_ready",
  );
  assertEquals(
    parsePreviewMessage({
      type: "hyper_term_preview_error",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 3,
      message: "bounded failure",
      generated_line: 2,
      generated_column: 4,
    }, channel)?.type,
    "hyper_term_preview_error",
  );
  assertEquals(
    parsePreviewMessage({
      type: "hyper_term_preview_trace",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 3,
      event: {
        schema_version: 1,
        stream_id: "2af0b724-c062-4fb8-a21e-b44f88809298",
        client_sequence: 1,
        kind: "checkpoint",
        name: "security.probe",
        payload: { denied: true },
      },
    }, channel)?.type,
    "hyper_term_preview_trace",
  );
  assertEquals(
    parsePreviewMessage({
      type: "hyper_term_preview_quality_capture",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 3,
      artifact_digest: "a".repeat(64),
      observation: {
        capture_id: "desktop-dark-reduced-motion",
        viewport: { width: 1_280, height: 800 },
        color_scheme: "dark",
        locale: "en",
        scenario: "default",
        reduced_motion: true,
        document_width: 1_280,
        document_height: 800,
        element_count: 12,
        interactive_count: 2,
        clipped_count: 0,
        undersized_target_count: 0,
        low_contrast_count: 0,
        hidden_primary_action_count: 0,
        console_error_count: 0,
        resource_failure_count: 0,
        layout_shift_milli: 0,
        semantic_digest: "b".repeat(64),
        samples: [],
      },
    }, channel)?.type,
    "hyper_term_preview_quality_capture",
  );
});

Deno.test("hostile preview envelopes fail closed without throwing", () => {
  const cycle: Record<string, unknown> = {};
  cycle.self = cycle;
  const rejected = [
    null,
    "message",
    [],
    cycle,
    {
      type: "hyper_term_preview_boot",
      schema_version: 1,
      channel_token: "wrong",
    },
    {
      type: "hyper_term_preview_ready",
      schema_version: 2,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 1,
    },
    {
      type: "hyper_term_preview_ready",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: -1,
    },
    {
      type: "hyper_term_preview_trace",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 1,
      event: { kind: "checkpoint" },
    },
    {
      type: "hyper_term_preview_quality_capture",
      schema_version: 1,
      channel_token: channel,
      artifact_id: artifact,
      source_revision: 1,
      artifact_digest: "not-a-digest",
      observation: {},
    },
    {
      type: "hyper_term_preview_boot",
      schema_version: 1,
      channel_token: channel,
      padding: "x".repeat(MAX_PREVIEW_MESSAGE_BYTES),
    },
  ];
  for (const value of rejected) {
    assertEquals(parsePreviewMessage(value, channel), undefined);
  }
});
