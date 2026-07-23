import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { VisualCaptureObservation } from "../../genui/visual-quality-measure.ts";
import { parsePreviewMessage } from "../genui/preview-message.ts";
import {
  type AcceptedRenderPayload,
  VisualQualityClient,
  type VisualQualityContext,
  type VisualQualityReport,
} from "../visual-quality-client.ts";

const CAPTURE_MATRIX = [
  {
    id: "narrow-light-default",
    width: 390,
    height: 844,
    colorScheme: "light",
    reducedMotion: false,
  },
  {
    id: "tablet-light-default",
    width: 768,
    height: 1_024,
    colorScheme: "light",
    reducedMotion: false,
  },
  {
    id: "desktop-light-default",
    width: 1_280,
    height: 800,
    colorScheme: "light",
    reducedMotion: false,
  },
  {
    id: "desktop-dark-default",
    width: 1_280,
    height: 800,
    colorScheme: "dark",
    reducedMotion: false,
  },
  {
    id: "desktop-light-reduced-motion",
    width: 1_280,
    height: 800,
    colorScheme: "light",
    reducedMotion: true,
  },
] as const;

interface CaptureRun {
  payload: AcceptedRenderPayload;
  index: number;
  channel: string;
  captures: VisualCaptureObservation[];
}

export function VisualQualityGate(props: VisualQualityContext) {
  const client = useMemo(() => new VisualQualityClient(props), [
    props.artifactId,
    props.sessionId,
    props.sourceRevision,
    props.token,
  ]);
  const [report, setReport] = useState<VisualQualityReport>();
  const [run, setRun] = useState<CaptureRun>();
  const [status, setStatus] = useState<
    "loading" | "capturing" | "ready" | "failed"
  >("loading");
  const [error, setError] = useState<string>();
  const iframe = useRef<HTMLIFrameElement>(null);
  const renderSent = useRef<string | undefined>(undefined);
  const measureSent = useRef<string | undefined>(undefined);
  const submitController = useRef<AbortController | undefined>(undefined);

  const beginCapture = useCallback(async () => {
    submitController.current?.abort();
    const controller = new AbortController();
    submitController.current = controller;
    setError(undefined);
    setStatus("capturing");
    setReport(undefined);
    renderSent.current = undefined;
    measureSent.current = undefined;
    try {
      const payload = await client.renderPayload(controller.signal);
      if (controller.signal.aborted) return;
      setRun({
        payload,
        index: 0,
        channel: crypto.randomUUID(),
        captures: [],
      });
    } catch (captureError) {
      if (controller.signal.aborted) return;
      setStatus("failed");
      setError(messageOf(captureError));
    }
  }, [client]);

  const sendAcceptedArtifact = useCallback((captureRun: CaptureRun) => {
    const target = iframe.current?.contentWindow;
    const renderKey =
      `${captureRun.channel}:${captureRun.payload.artifact_id}:${captureRun.payload.source_revision}`;
    if (!target || renderSent.current === renderKey) return;
    renderSent.current = renderKey;
    target.postMessage({
      type: "hyper_term_render_artifact",
      schema_version: 1,
      channel_token: captureRun.channel,
      artifact: captureRun.payload,
    }, "*");
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    submitController.current = controller;
    setStatus("loading");
    client.report(controller.signal).then((existing) => {
      if (controller.signal.aborted) return;
      if (existing) {
        setReport(existing);
        setStatus("ready");
      } else {
        void beginCapture();
      }
    }).catch((loadError: unknown) => {
      if (controller.signal.aborted) return;
      setStatus("failed");
      setError(messageOf(loadError));
    });
    return () => controller.abort();
  }, [beginCapture, client]);

  // The capture iframe is a local static document and can finish booting before
  // a passive effect runs. Install the channel listener during the commit so a
  // fast boot message cannot be lost and leave the gate stuck in `capturing`.
  useLayoutEffect(() => {
    if (!run) return;
    const receive = (event: MessageEvent) => {
      if (event.source !== iframe.current?.contentWindow) return;
      const message = parsePreviewMessage(event.data, run.channel);
      if (!message) return;
      if (message.type === "hyper_term_preview_boot") {
        sendAcceptedArtifact(run);
        return;
      }
      if (
        message.type === "hyper_term_preview_ready" &&
        message.artifact_id === run.payload.artifact_id &&
        message.source_revision === run.payload.source_revision &&
        measureSent.current !== CAPTURE_MATRIX[run.index].id
      ) {
        const capture = CAPTURE_MATRIX[run.index];
        measureSent.current = capture.id;
        iframe.current?.contentWindow?.postMessage({
          type: "hyper_term_measure_visual_quality",
          schema_version: 1,
          channel_token: run.channel,
          artifact_id: run.payload.artifact_id,
          source_revision: run.payload.source_revision,
          capture: {
            capture_id: capture.id,
            viewport: { width: capture.width, height: capture.height },
            color_scheme: capture.colorScheme,
            locale: "en",
            scenario: "default",
            reduced_motion: capture.reducedMotion,
          },
        }, "*");
        return;
      }
      if (
        message.type === "hyper_term_preview_error" &&
        message.artifact_id === run.payload.artifact_id &&
        message.source_revision === run.payload.source_revision
      ) {
        setStatus("failed");
        setError(message.message);
        setRun(undefined);
        return;
      }
      if (
        message.type !== "hyper_term_preview_quality_capture" ||
        message.artifact_id !== run.payload.artifact_id ||
        message.source_revision !== run.payload.source_revision ||
        message.artifact_digest !== run.payload.content_digest ||
        message.observation.capture_id !== CAPTURE_MATRIX[run.index].id
      ) return;
      const expected = CAPTURE_MATRIX[run.index];
      if (
        message.observation.viewport.width !== expected.width ||
        message.observation.viewport.height !== expected.height ||
        message.observation.color_scheme !== expected.colorScheme ||
        message.observation.reduced_motion !== expected.reducedMotion
      ) {
        setStatus("failed");
        setError(
          `Capture ${message.observation.capture_id} did not match its Rust-owned environment.`,
        );
        setRun(undefined);
        return;
      }
      const captures = [...run.captures, message.observation];
      const nextIndex = run.index + 1;
      if (nextIndex < CAPTURE_MATRIX.length) {
        renderSent.current = undefined;
        measureSent.current = undefined;
        setRun({
          ...run,
          index: nextIndex,
          channel: crypto.randomUUID(),
          captures,
        });
        return;
      }
      const controller = new AbortController();
      submitController.current = controller;
      setRun(undefined);
      client.submit(run.payload, captures, controller.signal).then((next) => {
        if (controller.signal.aborted) return;
        setReport(next);
        setStatus("ready");
      }).catch((submitError: unknown) => {
        if (controller.signal.aborted) return;
        setStatus("failed");
        setError(messageOf(submitError));
      });
    };
    globalThis.addEventListener("message", receive);
    return () => globalThis.removeEventListener("message", receive);
  }, [client, run, sendAcceptedArtifact]);

  useEffect(() => () => submitController.current?.abort(), []);

  const capture = run ? CAPTURE_MATRIX[run.index] : undefined;
  const previewUrl = capture && run
    ? visualQualityPreviewUrl(capture, run.channel)
    : undefined;
  const blocking =
    report?.findings.filter((finding) => finding.severity === "blocking") ?? [];
  const coverage =
    report?.findings.filter((finding) => finding.category === "coverage_gap") ??
      [];

  return (
    <section
      className={`visual-quality-gate ${report?.review_state ?? status}`}
      aria-label="Visual quality gate"
    >
      <header>
        <span className="visual-quality-indicator" />
        <strong>Visual quality</strong>
        <span>{qualityLabel(report, status, run?.index)}</span>
        <button
          type="button"
          disabled={status === "capturing" || status === "loading"}
          onClick={() => void beginCapture()}
          title="Re-run the fixed Rust-owned viewport matrix"
        >
          Recheck
        </button>
      </header>
      {error && <p role="alert">{error}</p>}
      {report && (
        <details>
          <summary>
            {report.captures.length} viewports · {blocking.length} blocking ·
            {" "}
            {coverage.length} gaps
          </summary>
          <div className="visual-quality-findings">
            {report.findings.map((finding) => (
              <article
                key={finding.finding_id}
                data-severity={finding.severity}
              >
                <strong>{finding.category.replaceAll("_", " ")}</strong>
                <span>{finding.explanation}</span>
                {finding.capture_id && <code>{finding.capture_id}</code>}
                {finding.sample && (
                  <code>
                    {finding.sample.semantic_path}
                    {finding.sample.rect
                      ? ` · ${finding.sample.rect.x},${finding.sample.rect.y} ${finding.sample.rect.width}×${finding.sample.rect.height}`
                      : ""}
                  </code>
                )}
              </article>
            ))}
          </div>
        </details>
      )}
      {capture && previewUrl && (
        <iframe
          key={run?.channel}
          ref={iframe}
          className="visual-quality-capture-frame"
          title={`Visual quality capture ${capture.id}`}
          aria-hidden="true"
          sandbox="allow-scripts"
          src={previewUrl}
          width={capture.width}
          height={capture.height}
          style={{ width: capture.width, height: capture.height }}
          onLoad={() => run && sendAcceptedArtifact(run)}
        />
      )}
    </section>
  );
}

function qualityLabel(
  report: VisualQualityReport | undefined,
  status: "loading" | "capturing" | "ready" | "failed",
  captureIndex?: number,
): string {
  if (report) {
    if (report.review_state === "needs_revision") return "needs revision";
    if (report.review_state === "needs_review") return "needs review";
    return "review ready";
  }
  if (status === "capturing") {
    return `checking ${Number(captureIndex ?? 0) + 1}/${CAPTURE_MATRIX.length}`;
  }
  return status;
}

function visualQualityPreviewUrl(
  capture: (typeof CAPTURE_MATRIX)[number],
  channel: string,
): string {
  const url = new URL("./genui/preview.html", document.baseURI);
  url.searchParams.set("quality_color_scheme", capture.colorScheme);
  url.searchParams.set(
    "quality_reduced_motion",
    capture.reducedMotion ? "reduce" : "no-preference",
  );
  url.hash = channel;
  return url.href;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
