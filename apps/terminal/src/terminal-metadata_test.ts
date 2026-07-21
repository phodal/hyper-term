import { assertEquals } from "@std/assert";
import {
  MAX_TERMINAL_CWD_BYTES,
  MAX_TERMINAL_TITLE_BYTES,
  normalizeTerminalTitle,
  parseTerminalCwdOsc,
  TerminalMetadataState,
} from "./terminal-metadata.ts";

Deno.test("terminal title metadata is safe, compact, and UTF-8 bounded", () => {
  assertEquals(
    normalizeTerminalTitle("\u001b]0;  cargo   test\u0007"),
    "]0; cargo test",
  );
  const title = normalizeTerminalTitle("终".repeat(200));
  assertEquals(
    new TextEncoder().encode(title ?? "").byteLength <=
      MAX_TERMINAL_TITLE_BYTES,
    true,
  );
});

Deno.test("OSC 7 accepts only local absolute file URLs", () => {
  assertEquals(
    parseTerminalCwdOsc("file:///Users/phodal/ai/hyper%20term"),
    "/Users/phodal/ai/hyper term",
  );
  assertEquals(
    parseTerminalCwdOsc("file://localhost/tmp/project"),
    "/tmp/project",
  );
  assertEquals(parseTerminalCwdOsc("file://remote.example/tmp/project"), null);
  assertEquals(parseTerminalCwdOsc("https://example.com/tmp/project"), null);
  assertEquals(parseTerminalCwdOsc("not a URL"), null);
  const cwd = parseTerminalCwdOsc(`file:///${"a".repeat(800)}`);
  assertEquals(
    new TextEncoder().encode(cwd ?? "").byteLength <= MAX_TERMINAL_CWD_BYTES,
    true,
  );
});

Deno.test("metadata snapshots are monotonic and keep title and cwd together", () => {
  const state = new TerminalMetadataState();
  assertEquals(state.current(), null);
  assertEquals(state.setTitle("cargo test"), {
    revision: 1,
    title: "cargo test",
    cwd: null,
  });
  assertEquals(state.setCwd("/tmp/project"), {
    revision: 2,
    title: "cargo test",
    cwd: "/tmp/project",
  });
  assertEquals(state.setCwd("/tmp/project"), null);
  assertEquals(state.current(), {
    revision: 2,
    title: "cargo test",
    cwd: "/tmp/project",
  });
  state.rebase(9);
  assertEquals(state.setTitle("cargo next"), {
    revision: 10,
    title: "cargo next",
    cwd: "/tmp/project",
  });
});
