import { assertAlmostEquals, assertEquals } from "@std/assert";
import {
  contrastRatio,
  parseCssColor,
  viewportMatches,
} from "./visual-quality-measure.ts";

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

Deno.test("visual quality capture waits for the exact requested viewport", () => {
  const expected = { width: 390, height: 844 };
  assertEquals(viewportMatches(1, 1, expected), false);
  assertEquals(viewportMatches(389.6, 844.4, expected), true);
  assertEquals(viewportMatches(390, 843, expected), false);
});
