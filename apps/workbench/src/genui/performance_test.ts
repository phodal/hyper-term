import { assertEquals } from "@std/assert";
import { GenUiPerformanceTracker } from "./performance.ts";

Deno.test("GenUI performance tracker separates warm phases without content", () => {
  const tracker = new GenUiPerformanceTracker();
  tracker.begin(1, 0, 32);
  tracker.candidateReady(1, 52);
  tracker.accepted(1, 55);
  tracker.previewReady(1, 70);
  tracker.begin(2, 100, 132);
  tracker.recordLongTask(125, 51);
  tracker.candidateReady(2, 150);
  tracker.accepted(2, 154);
  tracker.previewReady(2, 180);

  assertEquals(tracker.snapshot(), {
    schemaVersion: 1,
    pendingEdits: 0,
    samples: [
      {
        revision: 1,
        warm: false,
        editToPreviewMs: 70,
        settleMs: 32,
        compileMs: 20,
        acceptanceMs: 3,
        previewMs: 15,
        mainThreadLongTaskCount: 0,
        maxMainThreadLongTaskMs: 0,
      },
      {
        revision: 2,
        warm: true,
        editToPreviewMs: 80,
        settleMs: 32,
        compileMs: 18,
        acceptanceMs: 4,
        previewMs: 26,
        mainThreadLongTaskCount: 1,
        maxMainThreadLongTaskMs: 51,
      },
    ],
    warm: {
      count: 1,
      p50EditToPreviewMs: 80,
      p95EditToPreviewMs: 80,
      maxEditToPreviewMs: 80,
      mainThreadLongTaskCount: 1,
      maxMainThreadLongTaskMs: 51,
    },
  });
});

Deno.test("GenUI performance tracker rejects incomplete and cancelled revisions", () => {
  const tracker = new GenUiPerformanceTracker();
  tracker.begin(0, 0, 1);
  tracker.begin(1, 10, 9);
  tracker.begin(2, 10, 12);
  tracker.candidateReady(2, 11);
  tracker.accepted(2, 13);
  assertEquals(tracker.previewReady(2, 14), undefined);
  tracker.cancel(2);
  tracker.recordLongTask(0, 49);

  assertEquals(tracker.snapshot(), {
    schemaVersion: 1,
    pendingEdits: 0,
    samples: [],
    warm: {
      count: 0,
      p50EditToPreviewMs: null,
      p95EditToPreviewMs: null,
      maxEditToPreviewMs: null,
      mainThreadLongTaskCount: 0,
      maxMainThreadLongTaskMs: 0,
    },
  });
});

Deno.test("GenUI performance evidence stays bounded to the latest 64 samples", () => {
  const tracker = new GenUiPerformanceTracker();
  for (let revision = 1; revision <= 70; revision += 1) {
    const start = revision * 100;
    tracker.begin(revision, start, start + 10);
    tracker.candidateReady(revision, start + 20);
    tracker.accepted(revision, start + 21);
    tracker.previewReady(revision, start + 30);
  }
  const snapshot = tracker.snapshot();
  assertEquals(snapshot.samples.length, 64);
  assertEquals(snapshot.samples[0].revision, 7);
  assertEquals(snapshot.samples.at(-1)?.revision, 70);
  assertEquals(snapshot.warm.count, 64);
});
