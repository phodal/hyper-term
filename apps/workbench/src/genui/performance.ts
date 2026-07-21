const MAX_PERFORMANCE_SAMPLES = 64;
const MAX_LONG_TASKS = 128;

interface PendingEdit {
  revision: number;
  editStartedAt: number;
  compileStartedAt: number;
  candidateReadyAt?: number;
  acceptedAt?: number;
}

interface LongTask {
  startAt: number;
  durationMs: number;
}

export interface GenUiPerformanceSample {
  revision: number;
  warm: boolean;
  editToPreviewMs: number;
  settleMs: number;
  compileMs: number;
  acceptanceMs: number;
  previewMs: number;
  mainThreadLongTaskCount: number;
  maxMainThreadLongTaskMs: number;
}

export interface GenUiPerformanceAggregate {
  count: number;
  p50EditToPreviewMs: number | null;
  p95EditToPreviewMs: number | null;
  maxEditToPreviewMs: number | null;
  mainThreadLongTaskCount: number;
  maxMainThreadLongTaskMs: number;
}

export interface GenUiPerformanceSnapshot {
  schemaVersion: 1;
  pendingEdits: number;
  samples: GenUiPerformanceSample[];
  warm: GenUiPerformanceAggregate;
}

/** Bounded timing-only evidence. Source, bundle, paths, and diagnostics stay out. */
export class GenUiPerformanceTracker {
  readonly #pending = new Map<number, PendingEdit>();
  readonly #samples: GenUiPerformanceSample[] = [];
  readonly #longTasks: LongTask[] = [];

  begin(
    revision: number,
    editStartedAt: number,
    compileStartedAt: number,
  ): void {
    if (
      !positiveSafeInteger(revision) || !validTime(editStartedAt) ||
      !validTime(compileStartedAt) || compileStartedAt < editStartedAt
    ) return;
    this.#pending.set(revision, {
      revision,
      editStartedAt,
      compileStartedAt,
    });
  }

  candidateReady(revision: number, at: number): void {
    const pending = this.#pending.get(revision);
    if (!pending || !validTime(at) || at < pending.compileStartedAt) return;
    pending.candidateReadyAt = at;
  }

  accepted(revision: number, at: number): void {
    const pending = this.#pending.get(revision);
    if (
      !pending || pending.candidateReadyAt === undefined || !validTime(at) ||
      at < pending.candidateReadyAt
    ) return;
    pending.acceptedAt = at;
  }

  previewReady(
    revision: number,
    at: number,
  ): GenUiPerformanceSample | undefined {
    const pending = this.#pending.get(revision);
    if (
      !pending || pending.candidateReadyAt === undefined ||
      pending.acceptedAt === undefined || !validTime(at) ||
      at < pending.acceptedAt
    ) return undefined;
    this.#pending.delete(revision);
    const overlappingTasks = this.#longTasks.filter((task) =>
      task.startAt < at &&
      task.startAt + task.durationMs > pending.editStartedAt
    );
    const sample: GenUiPerformanceSample = {
      revision,
      warm: this.#samples.length > 0,
      editToPreviewMs: duration(pending.editStartedAt, at),
      settleMs: duration(pending.editStartedAt, pending.compileStartedAt),
      compileMs: duration(pending.compileStartedAt, pending.candidateReadyAt),
      acceptanceMs: duration(pending.candidateReadyAt, pending.acceptedAt),
      previewMs: duration(pending.acceptedAt, at),
      mainThreadLongTaskCount: overlappingTasks.length,
      maxMainThreadLongTaskMs: maximum(
        overlappingTasks.map((task) => task.durationMs),
      ) ?? 0,
    };
    this.#samples.push(sample);
    if (this.#samples.length > MAX_PERFORMANCE_SAMPLES) this.#samples.shift();
    return { ...sample };
  }

  cancel(revision: number): void {
    this.#pending.delete(revision);
  }

  recordLongTask(startAt: number, durationMs: number): void {
    if (!validTime(startAt) || !validTime(durationMs) || durationMs < 50) {
      return;
    }
    this.#longTasks.push({ startAt, durationMs });
    if (this.#longTasks.length > MAX_LONG_TASKS) this.#longTasks.shift();
  }

  snapshot(): GenUiPerformanceSnapshot {
    const samples = this.#samples.map((sample) => ({ ...sample }));
    return {
      schemaVersion: 1,
      pendingEdits: this.#pending.size,
      samples,
      warm: aggregate(samples.filter((sample) => sample.warm)),
    };
  }
}

function aggregate(
  samples: GenUiPerformanceSample[],
): GenUiPerformanceAggregate {
  const durations = samples.map((sample) => sample.editToPreviewMs);
  return {
    count: samples.length,
    p50EditToPreviewMs: percentile(durations, 0.5),
    p95EditToPreviewMs: percentile(durations, 0.95),
    maxEditToPreviewMs: maximum(durations),
    mainThreadLongTaskCount: samples.reduce(
      (total, sample) => total + sample.mainThreadLongTaskCount,
      0,
    ),
    maxMainThreadLongTaskMs: maximum(
      samples.map((sample) => sample.maxMainThreadLongTaskMs),
    ) ?? 0,
  };
}

function percentile(values: number[], quantile: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(sorted.length * quantile) - 1)];
}

function maximum(values: number[]): number | null {
  return values.length === 0 ? null : Math.max(...values);
}

function duration(start: number, end: number): number {
  return Math.round(Math.max(0, end - start) * 100) / 100;
}

function validTime(value: number): boolean {
  return Number.isFinite(value) && value >= 0;
}

function positiveSafeInteger(value: number): boolean {
  return Number.isSafeInteger(value) && value > 0;
}

declare global {
  interface Window {
    __hyperTermGenUiDiagnostics?: () => GenUiPerformanceSnapshot;
  }
}
