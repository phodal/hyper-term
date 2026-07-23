import { assertAlmostEquals, assertEquals } from "@std/assert";
import {
  binaryEvidence,
  contrastRatio,
  declaredStateSemanticEvidence,
  focusIndicatorChanged,
  parseCssColor,
  viewportMatches,
} from "./visual-quality-measure.ts";

Deno.test("visual quality optional evidence always serializes as a finite count", () => {
  assertEquals(binaryEvidence(undefined), 0);
  assertEquals(binaryEvidence(false), 0);
  assertEquals(binaryEvidence(true), 1);
});

Deno.test("declared visual states require state-specific accessible feedback", () => {
  const base = { feedback: true, busy: false, alert: false, disabled: false };
  assertEquals(declaredStateSemanticEvidence("empty", base), 1);
  assertEquals(declaredStateSemanticEvidence("loading", base), 0);
  assertEquals(
    declaredStateSemanticEvidence("loading", { ...base, busy: true }),
    1,
  );
  assertEquals(
    declaredStateSemanticEvidence("error", { ...base, alert: true }),
    1,
  );
  assertEquals(
    declaredStateSemanticEvidence("disabled", { ...base, disabled: true }),
    1,
  );
  assertEquals(
    declaredStateSemanticEvidence("error", { ...base, feedback: false }),
    0,
  );
});

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

Deno.test("visual quality focus evidence requires a visible style change", () => {
  const idle = {
    outlineStyle: "none",
    outlineWidth: "0px",
    outlineColor: "rgb(0, 0, 0)",
    outlineOffset: "0px",
    boxShadow: "none",
    borderColor: "rgb(80, 80, 80)",
    backgroundColor: "rgb(255, 255, 255)",
    color: "rgb(0, 0, 0)",
  };
  assertEquals(focusIndicatorChanged(idle, idle), false);
  assertEquals(
    focusIndicatorChanged(idle, {
      ...idle,
      outlineStyle: "solid",
      outlineWidth: "3px",
      outlineColor: "rgb(132, 204, 22)",
    }),
    true,
  );
});

Deno.test("visual quality capture waits for the exact requested viewport", () => {
  const expected = { width: 390, height: 844 };
  assertEquals(viewportMatches(1, 1, expected), false);
  assertEquals(viewportMatches(389.6, 844.4, expected), true);
  assertEquals(viewportMatches(390, 843, expected), false);
});
