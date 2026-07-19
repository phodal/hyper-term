export type RuntimeTraceKind = "action" | "checkpoint" | "console" | "error";

export interface RuntimeTraceInput {
  schema_version: 1;
  stream_id: string;
  client_sequence: number;
  kind: RuntimeTraceKind;
  name: string;
  payload: unknown;
}

export interface RuntimeTraceEvent extends RuntimeTraceInput {
  event_sequence: number;
  artifact_id: string;
  source_revision: number;
  payload_digest: string;
  redacted: boolean;
  recorded_at_ms: number;
}

export interface RuntimeTraceProjection {
  artifact_id: string;
  source_revision: number;
  events: RuntimeTraceEvent[];
}

export interface RuntimeTraceContext {
  artifactId: string;
  sourceRevision: number;
  sessionId: number;
  token: string;
}

type Fetch = typeof globalThis.fetch;

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const TRACE_KINDS = new Set<RuntimeTraceKind>([
  "action",
  "checkpoint",
  "console",
  "error",
]);
const MAX_TRACE_BATCH = 16;
const MAX_TRACE_EVENTS = 256;
const MAX_TRACE_NAME_BYTES = 128;
const MAX_TRACE_EVENT_BYTES = 32 * 1024;

export class RuntimeTraceClient {
  constructor(
    private readonly context: RuntimeTraceContext,
    private readonly fetcher: Fetch = (input, init) =>
      globalThis.fetch(input, init),
  ) {}

  async list(signal?: AbortSignal): Promise<RuntimeTraceProjection> {
    return await this.request("GET", undefined, signal);
  }

  async append(
    events: RuntimeTraceInput[],
    signal?: AbortSignal,
  ): Promise<RuntimeTraceProjection> {
    if (
      events.length === 0 || events.length > MAX_TRACE_BATCH ||
      !events.every(validInput)
    ) {
      throw new Error("Runtime trace batch is invalid.");
    }
    return await this.request("POST", {
      source_revision: this.context.sourceRevision,
      events,
    }, signal);
  }

  private async request(
    method: "GET" | "POST",
    body: unknown,
    signal?: AbortSignal,
  ): Promise<RuntimeTraceProjection> {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
    });
    const response = await this.fetcher(
      `/agent/artifact/${
        encodeURIComponent(this.context.artifactId)
      }/runtime-trace?${query}`,
      {
        method,
        cache: "no-store",
        signal,
        ...(body === undefined ? {} : {
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        }),
      },
    );
    if (!response.ok) {
      throw new Error(
        response.status === 409
          ? "Runtime trace stream is stale; reload the current Artifact."
          : `Rust runtime trace endpoint returned ${response.status}.`,
      );
    }
    const projection = await response.json() as RuntimeTraceProjection;
    if (!validProjection(projection, this.context)) {
      throw new Error("Rust runtime trace projection violated its contract.");
    }
    return projection;
  }
}

export function isRuntimeTraceMessage(
  value: unknown,
): value is RuntimeTraceInput {
  return validInput(value);
}

function validProjection(
  projection: RuntimeTraceProjection,
  context: RuntimeTraceContext,
): boolean {
  if (
    !projection || typeof projection !== "object" ||
    projection.artifact_id !== context.artifactId ||
    projection.source_revision !== context.sourceRevision ||
    !Array.isArray(projection.events) ||
    projection.events.length > MAX_TRACE_EVENTS ||
    !projection.events.every((event) =>
      validInput(event) &&
      event.artifact_id === context.artifactId &&
      event.source_revision === context.sourceRevision &&
      Number.isSafeInteger(event.event_sequence) &&
      event.event_sequence >= 1 &&
      SHA256_PATTERN.test(event.payload_digest) &&
      typeof event.redacted === "boolean" &&
      Number.isSafeInteger(event.recorded_at_ms) &&
      event.recorded_at_ms >= 1
    )
  ) return false;
  return projection.events.every((event, index) =>
    index === 0 ||
    event.event_sequence === projection.events[index - 1].event_sequence + 1
  );
}

function validInput(value: unknown): value is RuntimeTraceInput {
  if (!value || typeof value !== "object") return false;
  const input = value as Partial<RuntimeTraceInput>;
  if (
    input.schema_version !== 1 || typeof input.stream_id !== "string" ||
    !UUID_PATTERN.test(input.stream_id) ||
    !Number.isSafeInteger(input.client_sequence) ||
    Number(input.client_sequence) < 1 ||
    typeof input.kind !== "string" ||
    !TRACE_KINDS.has(input.kind as RuntimeTraceKind) ||
    typeof input.name !== "string" || input.name.length === 0 ||
    new TextEncoder().encode(input.name).byteLength > MAX_TRACE_NAME_BYTES ||
    input.name.includes("\0") || input.name.includes("\n") ||
    input.name.includes("\r") || !("payload" in input)
  ) return false;
  try {
    const encoded = JSON.stringify(input);
    return typeof encoded === "string" &&
      new TextEncoder().encode(encoded).byteLength <= MAX_TRACE_EVENT_BYTES;
  } catch {
    return false;
  }
}
