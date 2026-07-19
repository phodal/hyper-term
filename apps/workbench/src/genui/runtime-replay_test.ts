import { assertEquals, assertThrows } from "@std/assert";
import type { RuntimeTraceEvent } from "../runtime-trace-client.ts";
import {
  canonicalReplayDigestInput,
  runReplayableEffect,
  RuntimeReplaySession,
  verifyReplayProjectionDigest,
} from "./runtime-replay.ts";

const artifactId = "55555555-5555-4555-8555-555555555555";
const streamId = "77777777-7777-4777-8777-777777777777";

function event(
  eventSequence: number,
  kind: RuntimeTraceEvent["kind"],
  name: string,
  payload: unknown,
  redacted = false,
): RuntimeTraceEvent {
  return {
    schema_version: 1,
    stream_id: streamId,
    client_sequence: eventSequence,
    event_sequence: eventSequence,
    artifact_id: artifactId,
    source_revision: 7,
    kind,
    name,
    payload,
    payload_digest: eventSequence.toString(16).padStart(64, "0"),
    redacted,
    recorded_at_ms: 1_753_000_000_000 + eventSequence,
  };
}

Deno.test("runtime replay rebuilds reducer state and substitutes effect receipts", async () => {
  let liveEffects = 0;
  const session = new RuntimeReplaySession(
    [
      event(1, "checkpoint", "counter", { state: { count: 2 } }),
      event(2, "action", "counter", { action: { type: "increment" } }),
      event(3, "effect_receipt", "weather.lookup", {
        input: { city: "Shanghai" },
        outcome: "succeeded",
        output: { temperature: 31 },
      }),
    ],
    3,
    "a".repeat(64),
  );

  const state = session.reduce(
    "counter",
    { count: 0 },
    (current, action: { type: string }) => {
      assertEquals(liveEffects, 0);
      return action.type === "increment"
        ? { count: current.count + 1 }
        : current;
    },
  );
  assertEquals(state, { count: 3 });
  const result = await runReplayableEffect(
    session,
    "weather.lookup",
    { city: "Shanghai" },
    () => {
      liveEffects += 1;
      return { temperature: 99 };
    },
    () => {
      throw new Error("replay must not record a new receipt");
    },
  );
  assertEquals(result, {
    temperature: 31,
  });
  assertEquals(liveEffects, 0);
});

Deno.test("runtime replay fails closed for missing redacted and failed receipts", () => {
  const failed = new RuntimeReplaySession(
    [
      event(1, "effect_receipt", "lookup", {
        input: {},
        outcome: "failed",
        error: "offline",
      }),
    ],
    1,
    "b".repeat(64),
  );
  assertThrows(() => failed.effect("lookup", {}), Error, "offline");

  const redacted = new RuntimeReplaySession(
    [
      event(1, "effect_receipt", "lookup", {
        input: {},
        outcome: "succeeded",
        output: "[REDACTED]",
      }, true),
    ],
    1,
    "c".repeat(64),
  );
  assertThrows(() => redacted.effect("lookup", {}), Error, "redacted");
  assertThrows(() => redacted.effect("missing", {}), Error, "No matching");
});

Deno.test("runtime replay verifies the Rust canonical projection digest", async () => {
  const events = [
    event(1, "action", "counter", { action: { type: "increment" } }),
    event(2, "console", "debug", { message: "not canonical state" }),
  ];
  const canonical = canonicalReplayDigestInput(7, events);
  const bytes = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(canonical),
  );
  const expected = [...new Uint8Array(bytes)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  assertEquals(await verifyReplayProjectionDigest(7, events, expected), true);
  assertEquals(
    await verifyReplayProjectionDigest(7, events, "0".repeat(64)),
    false,
  );
});
