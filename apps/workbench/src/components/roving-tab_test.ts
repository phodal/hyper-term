import { assertEquals } from "@std/assert";
import { nextHorizontalTabIndex } from "./roving-tab.ts";

Deno.test("horizontal tabs wrap and support Home and End", () => {
  assertEquals(nextHorizontalTabIndex(3, 0, "ArrowLeft"), 2);
  assertEquals(nextHorizontalTabIndex(3, 2, "ArrowRight"), 0);
  assertEquals(nextHorizontalTabIndex(3, 1, "Home"), 0);
  assertEquals(nextHorizontalTabIndex(3, 1, "End"), 2);
});

Deno.test("horizontal tabs ignore unrelated keys and empty lists", () => {
  assertEquals(nextHorizontalTabIndex(3, 1, "Enter"), undefined);
  assertEquals(nextHorizontalTabIndex(0, 0, "ArrowRight"), undefined);
});
