import { assertEquals } from "@std/assert";
import { sameDayRelease } from "./check_release_cadence.ts";

const releases = [
  {
    tagName: "v0.1.0-rc.25",
    publishedAt: "2026-07-20T17:41:41Z",
  },
  {
    tagName: "v0.1.0-rc.23",
    publishedAt: "2026-07-20T16:53:16Z",
  },
];

Deno.test("release cadence permits only one new tag per Shanghai day", () => {
  const now = new Date("2026-07-21T03:00:00Z");
  assertEquals(
    sameDayRelease(releases, "v0.1.0-rc.26", now, "Asia/Shanghai")
      ?.tagName,
    "v0.1.0-rc.25",
  );
});

Deno.test("release cadence permits the same tag to be repaired", () => {
  const now = new Date("2026-07-21T03:00:00Z");
  assertEquals(
    sameDayRelease(releases, "v0.1.0-rc.25", now, "Asia/Shanghai"),
    undefined,
  );
});

Deno.test("release cadence resets at the Shanghai day boundary", () => {
  const now = new Date("2026-07-21T16:00:00Z");
  assertEquals(
    sameDayRelease(releases, "v0.1.0-rc.26", now, "Asia/Shanghai"),
    undefined,
  );
});
