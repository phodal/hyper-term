export interface TerminalDiagnosticsSnapshot {
  outputBytes: number;
  outputFrames: number;
  renderEvents: number;
  resizeEvents: number;
  lastSequence: number;
  lastOutputAt: number;
}

/** Bounded counters for release probes; terminal content is never exposed. */
export class TerminalDiagnostics {
  #outputBytes = 0;
  #outputFrames = 0;
  #renderEvents = 0;
  #resizeEvents = 0;
  #lastSequence = 0;
  #lastOutputAt = 0;

  recordOutput(byteLength: number, sequence: bigint, now: number): void {
    this.#outputBytes += Math.max(0, byteLength);
    this.#outputFrames += 1;
    this.#lastSequence = safeSequence(sequence);
    this.#lastOutputAt = now;
  }

  recordRender(): void {
    this.#renderEvents += 1;
  }

  recordResize(): void {
    this.#resizeEvents += 1;
  }

  snapshot(): TerminalDiagnosticsSnapshot {
    return {
      outputBytes: this.#outputBytes,
      outputFrames: this.#outputFrames,
      renderEvents: this.#renderEvents,
      resizeEvents: this.#resizeEvents,
      lastSequence: this.#lastSequence,
      lastOutputAt: this.#lastOutputAt,
    };
  }
}

function safeSequence(sequence: bigint): number {
  return Number(
    sequence > BigInt(Number.MAX_SAFE_INTEGER) ? 0n : sequence,
  );
}
