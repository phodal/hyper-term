import { assertEquals } from "@std/assert";
import {
  applyWorkbenchColorScheme,
  workbenchColorScheme,
} from "./workbench-theme.ts";

Deno.test("workbench theme follows the system color scheme", () => {
  assertEquals(workbenchColorScheme(false), "dark");
  assertEquals(workbenchColorScheme(true), "light");

  const target = { dataset: {} as DOMStringMap };
  applyWorkbenchColorScheme(target, "light");
  assertEquals(target.dataset.theme, "light");
  applyWorkbenchColorScheme(target, "dark");
  assertEquals(target.dataset.theme, "dark");
});
