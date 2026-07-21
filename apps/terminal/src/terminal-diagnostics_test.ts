import { assertEquals } from "@std/assert";
import { TerminalDiagnostics } from "./terminal-diagnostics.ts";

Deno.test("terminal diagnostics expose counters without terminal content", () => {
  const diagnostics = new TerminalDiagnostics();
  diagnostics.recordResize();
  diagnostics.recordRender();
  diagnostics.recordRender();
  diagnostics.recordOutput(1_024, 7n, 42.5);
  diagnostics.recordOutput(-1, 8n, 43.5);

  assertEquals(diagnostics.snapshot(), {
    outputBytes: 1_024,
    outputFrames: 2,
    renderEvents: 2,
    resizeEvents: 1,
    lastSequence: 8,
    lastOutputAt: 43.5,
  });
  assertEquals("content" in diagnostics.snapshot(), false);
});

Deno.test("terminal diagnostics fail closed on unsafe sequence numbers", () => {
  const diagnostics = new TerminalDiagnostics();
  diagnostics.recordOutput(1, BigInt(Number.MAX_SAFE_INTEGER) + 1n, 1);
  assertEquals(diagnostics.snapshot().lastSequence, 0);
});
