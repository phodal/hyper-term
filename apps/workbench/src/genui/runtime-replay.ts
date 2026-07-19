import type { RuntimeTraceEvent } from "../runtime-trace-client.ts";

export type ReplayReducer<State, Action> = (
  state: State,
  action: Action,
) => State;

export interface EffectReceiptPayload {
  input: unknown;
  outcome: "succeeded" | "failed";
  output?: unknown;
  error?: string;
}

export async function runReplayableEffect<T>(
  session: RuntimeReplaySession | undefined,
  name: string,
  input: unknown,
  invoke: () => T | Promise<T>,
  record: (payload: EffectReceiptPayload) => void,
): Promise<T> {
  if (session) return session.effect(name, input) as T;
  try {
    const output = await invoke();
    record({
      input: clone(input),
      outcome: "succeeded",
      output: clone(output),
    });
    return output;
  } catch (error) {
    record({
      input: clone(input),
      outcome: "failed",
      error: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
}

/**
 * A read-only replay session built exclusively from Rust-returned evidence.
 * It never owns an execution callback, so replay cannot accidentally fall
 * through to a live browser, shell, ACP, or MCP effect.
 */
export class RuntimeReplaySession {
  readonly events: RuntimeTraceEvent[];
  readonly targetEventSequence: number;
  readonly projectionDigest: string;
  private readonly effectOffsets = new Map<string, number>();

  constructor(
    events: RuntimeTraceEvent[],
    targetEventSequence: number,
    projectionDigest: string,
  ) {
    if (!/^[0-9a-f]{64}$/.test(projectionDigest)) {
      throw new Error("Replay projection digest is invalid.");
    }
    const targetIndex = events.findIndex((event) =>
      event.event_sequence === targetEventSequence
    );
    if (targetIndex < 0 || !isReplayBoundary(events[targetIndex])) {
      throw new Error("Replay target is not a deterministic event boundary.");
    }
    const selected = events.slice(0, targetIndex + 1);
    if (
      !selected.every((event, index) =>
        index === 0 ||
        event.event_sequence === selected[index - 1].event_sequence + 1
      )
    ) {
      throw new Error("Replay evidence is not contiguous.");
    }
    this.events = selected;
    this.targetEventSequence = targetEventSequence;
    this.projectionDigest = projectionDigest;
  }

  reduce<State, Action>(
    name: string,
    initialState: State,
    reducer: ReplayReducer<State, Action>,
  ): State {
    let state = clone(initialState) as State;
    for (const event of this.events) {
      if (event.name !== name) continue;
      if (event.kind === "checkpoint") {
        const payload = objectPayload(event.payload);
        if (!("state" in payload)) {
          throw new Error(`Checkpoint ${name} does not contain state.`);
        }
        state = clone(payload.state) as State;
      } else if (event.kind === "action") {
        const payload = objectPayload(event.payload);
        if (!("action" in payload)) {
          throw new Error(`Action ${name} does not contain an action payload.`);
        }
        state = reducer(state, clone(payload.action) as Action);
      }
    }
    return state;
  }

  effect(name: string, input: unknown): unknown {
    const inputKey = canonicalJson(input);
    const offset = this.effectOffsets.get(name) ?? 0;
    const receipts = this.events.filter((event) =>
      event.kind === "effect_receipt" && event.name === name
    );
    const receipt = receipts[offset];
    if (!receipt) {
      throw new Error(`No matching effect receipt is available for ${name}.`);
    }
    const payload = effectPayload(receipt.payload);
    if (canonicalJson(payload.input) !== inputKey) {
      throw new Error(`Effect receipt input order changed for ${name}.`);
    }
    this.effectOffsets.set(name, offset + 1);
    if (receipt.redacted) {
      throw new Error(
        `Effect receipt ${name} is redacted and cannot replay.`,
      );
    }
    if (payload.outcome === "failed") {
      throw new Error(payload.error ?? `Recorded effect ${name} failed.`);
    }
    if (!("output" in payload)) {
      throw new Error(`Effect receipt ${name} has no recorded output.`);
    }
    return clone(payload.output);
  }
}

export function isReplayBoundary(event: RuntimeTraceEvent): boolean {
  return event.kind === "action" || event.kind === "checkpoint" ||
    event.kind === "effect_receipt";
}

export function canonicalReplayDigestInput(
  sourceRevision: number,
  events: RuntimeTraceEvent[],
): string {
  return JSON.stringify([
    1,
    sourceRevision,
    events.filter(isReplayBoundary).map((event) => [
      event.event_sequence,
      event.stream_id,
      event.client_sequence,
      event.kind,
      event.name,
      event.payload_digest,
      event.redacted,
    ]),
  ]);
}

export async function verifyReplayProjectionDigest(
  sourceRevision: number,
  events: RuntimeTraceEvent[],
  expected: string,
): Promise<boolean> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(
      canonicalReplayDigestInput(sourceRevision, events),
    ),
  );
  const actual = [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  return actual === expected;
}

function effectPayload(value: unknown): EffectReceiptPayload {
  const payload = objectPayload(value) as Partial<EffectReceiptPayload>;
  if (
    !("input" in payload) ||
    (payload.outcome !== "succeeded" && payload.outcome !== "failed") ||
    (payload.outcome === "succeeded" && !("output" in payload)) ||
    (payload.outcome === "failed" && typeof payload.error !== "string")
  ) {
    throw new Error("Recorded effect receipt violated its replay contract.");
  }
  return payload as EffectReceiptPayload;
}

function objectPayload(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Runtime replay payload must be an object.");
  }
  return value as Record<string, unknown>;
}

function canonicalJson(value: unknown): string {
  return JSON.stringify(sortValue(value));
}

function sortValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, child]) => [key, sortValue(child)]),
    );
  }
  return value;
}

function clone(value: unknown): unknown {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) {
    throw new Error("Runtime replay values must be JSON serializable.");
  }
  return JSON.parse(encoded);
}
