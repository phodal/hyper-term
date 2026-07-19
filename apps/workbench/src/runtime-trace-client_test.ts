import { assertEquals, assertRejects } from "@std/assert";
import {
  RuntimeTraceClient,
  type RuntimeTraceEvent,
  type RuntimeTraceInput,
} from "./runtime-trace-client.ts";

const artifactId = "55555555-5555-4555-8555-555555555555";
const streamId = "77777777-7777-4777-8777-777777777777";
const context = {
  artifactId,
  sourceRevision: 7,
  sessionId: 3,
  token: "0123456789abcdef0123456789abcdef",
};
const input: RuntimeTraceInput = {
  schema_version: 1,
  stream_id: streamId,
  client_sequence: 1,
  kind: "checkpoint",
  name: "counter.changed",
  payload: { count: 2 },
};

Deno.test("runtime trace client appends and reloads exact Rust evidence", async () => {
  const requests: Request[] = [];
  const event: RuntimeTraceEvent = {
    ...input,
    event_sequence: 1,
    artifact_id: artifactId,
    source_revision: 7,
    payload_digest: "a".repeat(64),
    redacted: false,
    recorded_at_ms: 1_753_000_000_001,
  };
  const client = new RuntimeTraceClient(context, (request, init) => {
    requests.push(
      new Request(new URL(String(request), "http://hyper.test"), init),
    );
    return Promise.resolve(Response.json({
      artifact_id: artifactId,
      source_revision: 7,
      projection_digest: "b".repeat(64),
      events: [event],
    }));
  });

  const appended = await client.append([input]);
  const loaded = await client.list();

  assertEquals(appended.events, [event]);
  assertEquals(loaded.events, [event]);
  assertEquals(requests[0].method, "POST");
  assertEquals(requests[1].method, "GET");
  assertEquals(new URL(requests[0].url).searchParams.get("session_id"), "3");
  assertEquals((await requests[0].json()).events, [input]);
});

Deno.test("runtime trace client rejects stale and malformed projections", async () => {
  const stale = new RuntimeTraceClient(
    context,
    () => Promise.resolve(new Response("stale", { status: 409 })),
  );
  await assertRejects(() => stale.append([input]), Error, "stream is stale");

  const malformed = new RuntimeTraceClient(
    context,
    () =>
      Promise.resolve(Response.json({
        artifact_id: artifactId,
        source_revision: 7,
        projection_digest: "b".repeat(64),
        events: [{ ...input, event_sequence: 1 }],
      })),
  );
  await assertRejects(() => malformed.list(), Error, "violated its contract");
});

Deno.test("runtime trace client bounds messages before network dispatch", async () => {
  let requestCount = 0;
  const client = new RuntimeTraceClient(
    context,
    () => {
      requestCount += 1;
      return Promise.reject(new Error("network must not run"));
    },
  );
  await assertRejects(
    () =>
      client.append([{
        ...input,
        name: "x".repeat(129),
      }]),
    Error,
    "batch is invalid",
  );
  const { payload: _payload, ...withoutPayload } = input;
  await assertRejects(
    () => client.append([withoutPayload as RuntimeTraceInput]),
    Error,
    "batch is invalid",
  );
  assertEquals(requestCount, 0);
});
