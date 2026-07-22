import { assertAlmostEquals, assertEquals } from "@std/assert";
import { contrastRatio, parseCssColor } from "./visual-quality-measure.ts";

Deno.test("visual quality contrast checker is deterministic", () => {
  assertEquals(parseCssColor("rgb(255, 255, 255)"), [255, 255, 255]);
  assertEquals(parseCssColor("rgba(0, 0, 0, 0.5)"), undefined);
  assertAlmostEquals(
    contrastRatio([0, 0, 0], [255, 255, 255]),
    21,
    0.001,
  );
  assertAlmostEquals(
    contrastRatio([119, 119, 119], [255, 255, 255]),
    4.478,
    0.01,
  );
});
