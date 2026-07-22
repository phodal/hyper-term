import { assertEquals } from "@std/assert";

import {
  defaultSourceLineLimit,
  sourceLineCount,
  sourceLineLimit,
} from "./check_source_size.ts";

Deno.test("source line counts match wc semantics", () => {
  assertEquals(sourceLineCount(""), 0);
  assertEquals(sourceLineCount("one"), 1);
  assertEquals(sourceLineCount("one\ntwo\n"), 2);
});

Deno.test("new files use the default budget while legacy hotspots are frozen", () => {
  assertEquals(defaultSourceLineLimit, 2_000);
  assertEquals(
    sourceLineLimit("apps/desktop/src/new_view.zig"),
    defaultSourceLineLimit,
  );
  assertEquals(
    sourceLineLimit("apps/desktop/src/main.zig"),
    defaultSourceLineLimit,
  );
  assertEquals(
    sourceLineLimit("crates/hyper-term-daemon/src/agent_gateway.rs"),
    5_298,
  );
  assertEquals(
    sourceLineLimit("crates/hyper-term-daemon/src/lib.rs"),
    3_452,
  );
  assertEquals(
    sourceLineLimit("crates/hyper-term-drivers/src/acp.rs"),
    2_655,
  );
});
