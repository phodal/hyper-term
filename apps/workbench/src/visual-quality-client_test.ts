import { assertEquals, assertRejects } from "@std/assert";
import {
  VisualQualityClient,
  type VisualQualityReport,
} from "./visual-quality-client.ts";

const context = {
  artifactId: "55555555-5555-4555-8555-555555555555",
  sourceRevision: 7,
  sessionId: 3,
  token: "0123456789abcdef0123456789abcdef",
};

const observation = (
  capture_id: string,
  width: number,
  height: number,
  color_scheme: "light" | "dark" = "light",
  reduced_motion = false,
  scenario:
    | "default"
    | "focus-first"
    | "content-stress"
    | "state-empty"
    | "state-loading"
    | "state-error"
    | "state-disabled" = "default",
  locale: "en" | "zh-CN" = "en",
) => ({
  capture_id,
  viewport: { width, height },
  color_scheme,
  locale,
  scenario,
  reduced_motion,
  document_width: width,
  document_height: height,
  element_count: 8,
  interactive_count: 1,
  clipped_count: 0,
  undersized_target_count: 0,
  low_contrast_count: 0,
  hidden_primary_action_count: 0,
  focus_target_count: scenario === "focus-first" ? 1 : 0,
  focus_visible_count: scenario === "focus-first" ? 1 : 0,
  ...(scenario === "content-stress"
    ? { content_fixture_digest: "9".repeat(64) }
    : {}),
  content_fixture_target_count: scenario === "content-stress" ? 2 : 0,
  content_fixture_applied_count: scenario === "content-stress" ? 2 : 0,
  content_fixture_cjk_label_count: scenario === "content-stress" ? 1 : 0,
  content_fixture_long_content_count: scenario === "content-stress" ? 1 : 0,
  ...(scenario.startsWith("state-")
    ? { declared_state_digest: "8".repeat(64) }
    : {}),
  declared_state_target_count: scenario.startsWith("state-") ? 1 : 0,
  declared_state_applied_count: scenario.startsWith("state-") ? 1 : 0,
  declared_state_semantic_count: scenario.startsWith("state-") ? 1 : 0,
  console_error_count: 0,
  resource_failure_count: 0,
  layout_shift_milli: 0,
  semantic_digest: "b".repeat(64),
  samples: [],
});

const captures = [
  observation("narrow-light-default", 390, 844),
  observation("tablet-light-default", 768, 1_024),
  observation("desktop-light-default", 1_280, 800),
  observation("desktop-dark-default", 1_280, 800, "dark"),
  observation("desktop-light-reduced-motion", 1_280, 800, "light", true),
  observation(
    "desktop-light-focus-first",
    1_280,
    800,
    "light",
    false,
    "focus-first",
  ),
  observation(
    "narrow-zh-content-stress",
    390,
    844,
    "light",
    false,
    "content-stress",
    "zh-CN",
  ),
  observation(
    "desktop-light-state-empty",
    1_280,
    800,
    "light",
    false,
    "state-empty",
  ),
  observation(
    "desktop-light-state-loading",
    1_280,
    800,
    "light",
    false,
    "state-loading",
  ),
  observation(
    "desktop-light-state-error",
    1_280,
    800,
    "light",
    false,
    "state-error",
  ),
  observation(
    "desktop-light-state-disabled",
    1_280,
    800,
    "light",
    false,
    "state-disabled",
  ),
];

function report(): VisualQualityReport {
  return {
    schema_version: 4,
    artifact_id: context.artifactId,
    source_revision: context.sourceRevision,
    artifact_digest: "a".repeat(64),
    preview_runtime_digest: "c".repeat(64),
    capture_manifest_digest: "d".repeat(64),
    checker_version: "hyper-term-objective-v5",
    captures: captures.map((observation) => ({
      ...observation,
      observation_digest: "e".repeat(64),
    })),
    findings: [{
      finding_id: "coverage:host-pixel-capture",
      category: "coverage_gap",
      severity: "warning",
      explanation: "Host pixel captures are pending.",
    }],
    objective_status: "passed",
    advisory_status: "not_run",
    review_state: "needs_review",
    report_digest: "f".repeat(64),
  };
}

Deno.test("visual quality client loads exact accepted payload and submits observations", async () => {
  const requests: Request[] = [];
  const client = new VisualQualityClient(context, (input, init) => {
    const request = new Request(
      new URL(String(input), "http://hyper.test"),
      init,
    );
    requests.push(request);
    if (request.url.includes("render-payload")) {
      return Promise.resolve(Response.json({
        artifact_id: context.artifactId,
        source_revision: context.sourceRevision,
        content_digest: "a".repeat(64),
        bundle: "export default 1",
        css: "",
        source_map: "",
      }));
    }
    return Promise.resolve(Response.json(report()));
  });
  const payload = await client.renderPayload();
  const accepted = await client.submit(payload, captures);
  assertEquals(accepted.review_state, "needs_review");
  assertEquals(requests[0].method, "GET");
  assertEquals(requests[1].method, "POST");
  assertEquals(new URL(requests[1].url).searchParams.get("session_id"), "3");
  const body = await requests[1].json();
  assertEquals(body.captures.length, 11);
  assertEquals(body.artifact_digest, "a".repeat(64));
});

Deno.test("visual quality client treats missing report separately and rejects stale reports", async () => {
  const missing = new VisualQualityClient(
    context,
    () => Promise.resolve(new Response("missing", { status: 404 })),
  );
  assertEquals(await missing.report(), undefined);

  const stale = new VisualQualityClient(
    context,
    () => Promise.resolve(Response.json({ ...report(), source_revision: 8 })),
  );
  await assertRejects(() => stale.report(), Error, "violated its contract");
});
