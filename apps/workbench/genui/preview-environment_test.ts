import { assertEquals } from "@std/assert";
import {
  previewQualityEnvironment,
  rewritePreferenceMediaQuery,
} from "./preview-environment.ts";

Deno.test("preview quality environment is explicit and bounded", () => {
  assertEquals(
    previewQualityEnvironment(new URL("https://preview.invalid/")),
    undefined,
  );
  assertEquals(
    previewQualityEnvironment(
      new URL(
        "https://preview.invalid/?quality_color_scheme=dark&quality_reduced_motion=reduce",
      ),
    ),
    { colorScheme: "dark", reducedMotion: true },
  );
  assertEquals(
    previewQualityEnvironment(
      new URL(
        "https://preview.invalid/?quality_color_scheme=system&quality_reduced_motion=reduce",
      ),
    ),
    undefined,
  );
});

Deno.test("preview media queries are rewritten to deterministic preferences", () => {
  const environment = { colorScheme: "dark", reducedMotion: true } as const;
  assertEquals(
    rewritePreferenceMediaQuery(
      "screen and (prefers-color-scheme: dark)",
      environment,
    ),
    "screen and (min-width: 0px)",
  );
  assertEquals(
    rewritePreferenceMediaQuery(
      "(prefers-color-scheme: light), (prefers-reduced-motion: reduce)",
      environment,
    ),
    "(max-width: -1px), (min-width: 0px)",
  );
  assertEquals(
    rewritePreferenceMediaQuery(
      "(prefers-reduced-motion: no-preference)",
      environment,
    ),
    "(max-width: -1px)",
  );
});
