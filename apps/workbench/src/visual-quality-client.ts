import type { VisualCaptureObservation } from "../genui/visual-quality-measure.ts";

export interface VisualQualityContext {
  artifactId: string;
  sourceRevision: number;
  sessionId: number;
  token: string;
}

export interface AcceptedRenderPayload {
  artifact_id: string;
  source_revision: number;
  content_digest: string;
  bundle: string;
  css: string;
  source_map: string;
}

export interface VisualQualityFinding {
  finding_id: string;
  category:
    | "empty_render"
    | "viewport_overflow"
    | "clipped_content"
    | "undersized_target"
    | "low_contrast"
    | "hidden_primary_action"
    | "missing_focus_indicator"
    | "console_error"
    | "resource_failure"
    | "layout_instability"
    | "coverage_gap";
  severity: "blocking" | "warning" | "info";
  capture_id?: string;
  explanation: string;
  sample?: {
    category: string;
    semantic_path: string;
    rect?: { x: number; y: number; width: number; height: number };
  };
}

export interface VisualQualityReport {
  schema_version: 2;
  artifact_id: string;
  source_revision: number;
  artifact_digest: string;
  preview_runtime_digest: string;
  capture_manifest_digest: string;
  checker_version: string;
  captures: Array<
    VisualCaptureObservation & {
      observation_digest: string;
      pixel_digest?: string;
    }
  >;
  findings: VisualQualityFinding[];
  objective_status: "passed" | "failed";
  advisory_status: "not_run" | "needs_review" | "clear";
  review_state: "needs_revision" | "needs_review" | "review_ready";
  report_digest: string;
}

export class VisualQualityClient {
  constructor(
    private readonly context: VisualQualityContext,
    private readonly fetcher: typeof fetch = globalThis.fetch.bind(globalThis),
  ) {}

  async renderPayload(signal?: AbortSignal): Promise<AcceptedRenderPayload> {
    const response = await this.fetcher(this.endpoint("render-payload"), {
      headers: { Accept: "application/json" },
      signal,
    });
    if (!response.ok) {
      throw new Error(
        `Rust accepted render payload returned ${response.status}.`,
      );
    }
    const payload: unknown = await response.json();
    if (!validRenderPayload(payload, this.context)) {
      throw new Error("Rust accepted render payload violated its contract.");
    }
    return payload;
  }

  async report(signal?: AbortSignal): Promise<VisualQualityReport | undefined> {
    const response = await this.fetcher(this.endpoint("visual-quality"), {
      headers: { Accept: "application/json" },
      signal,
    });
    if (response.status === 404) return undefined;
    if (!response.ok) {
      throw new Error(
        `Rust visual quality report returned ${response.status}.`,
      );
    }
    const report: unknown = await response.json();
    if (!validReport(report, this.context)) {
      throw new Error("Rust visual quality report violated its contract.");
    }
    return report;
  }

  async submit(
    payload: AcceptedRenderPayload,
    captures: VisualCaptureObservation[],
    signal?: AbortSignal,
  ): Promise<VisualQualityReport> {
    const response = await this.fetcher(this.endpoint("visual-quality"), {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify({
        schema_version: 2,
        source_revision: payload.source_revision,
        artifact_digest: payload.content_digest,
        captures,
      }),
      signal,
    });
    if (!response.ok) {
      throw new Error(
        `Rust visual quality submission returned ${response.status}.`,
      );
    }
    const report: unknown = await response.json();
    if (!validReport(report, this.context)) {
      throw new Error("Rust visual quality result violated its contract.");
    }
    return report;
  }

  private endpoint(resource: "render-payload" | "visual-quality"): string {
    return `/agent/artifact/${
      encodeURIComponent(this.context.artifactId)
    }/${resource}` +
      `?token=${encodeURIComponent(this.context.token)}` +
      `&session_id=${this.context.sessionId}`;
  }
}

function validRenderPayload(
  value: unknown,
  context: VisualQualityContext,
): value is AcceptedRenderPayload {
  if (!record(value)) return false;
  return value.artifact_id === context.artifactId &&
    value.source_revision === context.sourceRevision &&
    sha256(value.content_digest) && boundedString(value.bundle, 2_000_000) &&
    boundedString(value.css, 2_000_000, true) &&
    boundedString(value.source_map, 2_000_000, true);
}

function validReport(
  value: unknown,
  context: VisualQualityContext,
): value is VisualQualityReport {
  if (!record(value)) return false;
  const states = new Set(["needs_revision", "needs_review", "review_ready"]);
  const objective = new Set(["passed", "failed"]);
  const advisory = new Set(["not_run", "needs_review", "clear"]);
  return value.schema_version === 2 &&
    value.artifact_id === context.artifactId &&
    value.source_revision === context.sourceRevision &&
    sha256(value.artifact_digest) &&
    sha256(value.preview_runtime_digest) &&
    sha256(value.capture_manifest_digest) &&
    sha256(value.report_digest) && boundedString(value.checker_version, 64) &&
    typeof value.review_state === "string" && states.has(value.review_state) &&
    typeof value.objective_status === "string" &&
    objective.has(value.objective_status) &&
    typeof value.advisory_status === "string" &&
    advisory.has(value.advisory_status) &&
    Array.isArray(value.captures) && value.captures.length === 6 &&
    value.captures.every(validCapture) &&
    Array.isArray(value.findings) && value.findings.length <= 64 &&
    value.findings.every(validFinding);
}

function validCapture(value: unknown): boolean {
  if (!record(value)) return false;
  return sha256(value.observation_digest) &&
    boundedString(value.capture_id, 64) && sha256(value.semantic_digest) &&
    (value.scenario === "default" || value.scenario === "focus-first") &&
    nonNegativeInteger(value.focus_target_count, 1) &&
    nonNegativeInteger(value.focus_visible_count, 1) &&
    Number(value.focus_visible_count) <= Number(value.focus_target_count) &&
    (value.pixel_digest === undefined || sha256(value.pixel_digest));
}

function validFinding(value: unknown): boolean {
  if (!record(value)) return false;
  return boundedString(value.finding_id, 128) &&
    boundedString(value.explanation, 1_024) &&
    ["blocking", "warning", "info"].includes(String(value.severity)) &&
    (value.capture_id === undefined || boundedString(value.capture_id, 64));
}

function record(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function sha256(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}

function nonNegativeInteger(value: unknown, maximum: number): boolean {
  return Number.isSafeInteger(value) && Number(value) >= 0 &&
    Number(value) <= maximum;
}

function boundedString(
  value: unknown,
  maximum: number,
  allowEmpty = false,
): value is string {
  return typeof value === "string" && (allowEmpty || value.length > 0) &&
    new TextEncoder().encode(value).byteLength <= maximum;
}
