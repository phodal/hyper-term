import { useEffect, useMemo, useRef, useState } from "react";
import type { ArtifactDraftStatus } from "../artifact-draft-publisher.ts";
import type {
  ArtifactEditorSelection,
  ArtifactEditorView,
} from "../artifact-editor-checkpoint.ts";
import type {
  ArtifactHistoryEntry,
  ArtifactHistorySource,
} from "../artifact-history-client.ts";
import type { HyperTermHost } from "../host.ts";
import type { AcceptedArtifact } from "../protocol.ts";
import {
  type RuntimeTraceEvent,
  type RuntimeTraceInput,
} from "../runtime-trace-client.ts";
import { parsePreviewMessage } from "../genui/preview-message.ts";
import { GenUiPerformanceTracker } from "../genui/performance.ts";
import { isReplayBoundary } from "../genui/runtime-replay.ts";
import type { EditorLanguageService } from "../editor-language-service.ts";
import { GenUiCompiler } from "../genui/compiler-client.ts";
import {
  mapPreviewRuntimeError,
  type RuntimeDiagnostic,
} from "../genui/runtime-diagnostic.ts";
import { CodeDiff } from "./code-diff.tsx";
import { CodeEditor } from "./code-editor.tsx";
import { ArtifactFileTabs } from "./artifact-file-tabs.tsx";
import {
  type BugCapsule,
  downloadBugCapsule,
} from "../debug-capsule-client.ts";

const sampleOriginalSource = `import React from "react";

export default function AgentStatus() {
  return (
    <main style={{ padding: 28 }}>
      <p>Agent is working…</p>
    </main>
  );
}
`;

const sampleInitialSource = `import React from "react";
import { traceCheckpoint, useReplayReducer } from "@hyper/runtime";

export default function AgentStatus() {
  const [state, dispatch] = useReplayReducer(
    "evidence.panel",
    (current, action) => action.type === "toggle"
      ? { expanded: !current.expanded }
      : current,
    { expanded: false },
  );
  return (
    <main style={{ padding: 28, fontFamily: "system-ui" }}>
      <section style={{
        border: "1px solid #39422e",
        borderRadius: 16,
        padding: 20,
        background: "linear-gradient(145deg, #1c2118, #12150f)",
      }}>
        <small style={{ color: "#9ba88a" }}>ACP · CODEX</small>
        <h2 style={{ margin: "10px 0 4px" }}>Verification complete</h2>
        <p style={{ color: "#aeb6a1" }}>19 tests passed · 0 effects replayed</p>
        <button onClick={() => {
          const next = !state.expanded;
          dispatch({ type: "toggle" });
          traceCheckpoint("evidence.panel", { state: { expanded: next } });
        }} style={{
          marginTop: 12,
          border: 0,
          borderRadius: 9,
          padding: "9px 13px",
          background: "#d7ff72",
          color: "#11140e",
          fontWeight: 700,
        }}>
          {state.expanded ? "Hide evidence" : "Show evidence"}
        </button>
        {state.expanded && <pre style={{ color: "#cbd5bc" }}>
          cargo test --workspace{"\\n"}deno task check{"\\n"}deno task build
        </pre>}
      </section>
    </main>
  );
}
`;

type StudioView = ArtifactEditorView;

const sampleBaselineFiles = { "/App.tsx": sampleOriginalSource };
const sampleDraftFiles = { "/App.tsx": sampleInitialSource };
const LIVE_BUILD_SETTLE_MS = 0;

interface TraceEntry {
  revision: number;
  label: string;
  detail: string;
  state: "working" | "accepted" | "failed";
}

export interface GenUiStudioProps {
  host: HyperTermHost;
  entrypoint?: string;
  initialFiles?: Record<string, string>;
  baselineFiles?: Record<string, string>;
  initialActivePath?: string;
  initialView?: StudioView;
  initialSelections?: Record<string, ArtifactEditorSelection>;
  initialRevision?: number;
  heading?: string;
  languageServiceForPath?: (path: string) => EditorLanguageService;
  onActivePathChange?: (path: string) => void;
  onPublishDraft?: (files: Record<string, string>) => void;
  onDraftStateChange?: (changed: boolean) => void;
  onCheckpointStateChange?: (state: {
    files: Record<string, string>;
    activePath: string;
    view: StudioView;
    selections: Record<string, ArtifactEditorSelection>;
  }) => void;
  checkpointStatus?: "restored" | "idle" | "saving" | "saved" | "failed";
  checkpointError?: string;
  publishStatus?: ArtifactDraftStatus | "idle";
  publishError?: string;
  historyEntries?: ArtifactHistoryEntry[];
  historyStatus?: "loading" | "ready" | "failed";
  historyError?: string;
  onLoadHistorySource?: (
    entry: ArtifactHistoryEntry,
    signal: AbortSignal,
  ) => Promise<ArtifactHistorySource>;
  runtimeTraceEvents?: RuntimeTraceEvent[];
  runtimeTraceProjectionDigest?: string;
  runtimeTraceStatus?: "loading" | "ready" | "saving" | "failed";
  runtimeTraceError?: string;
  onRuntimeTrace?: (event: RuntimeTraceInput) => void;
  onPrepareBugCapsule?: () => Promise<BugCapsule>;
}

export function GenUiStudio({
  host,
  entrypoint = "/App.tsx",
  initialFiles = sampleDraftFiles,
  baselineFiles = sampleBaselineFiles,
  initialActivePath = entrypoint,
  initialView = "code",
  initialSelections = {},
  initialRevision = 0,
  heading,
  languageServiceForPath,
  onActivePathChange,
  onPublishDraft,
  onDraftStateChange,
  onCheckpointStateChange,
  checkpointStatus = "idle",
  checkpointError,
  publishStatus = "idle",
  publishError,
  historyEntries = [],
  historyStatus = "ready",
  historyError,
  onLoadHistorySource,
  runtimeTraceEvents = [],
  runtimeTraceProjectionDigest = "",
  runtimeTraceStatus = "ready",
  runtimeTraceError,
  onRuntimeTrace,
  onPrepareBugCapsule,
}: GenUiStudioProps) {
  const [files, setFiles] = useState<Record<string, string>>(() => ({
    ...initialFiles,
  }));
  const [activePath, setActivePath] = useState(
    initialActivePath in initialFiles ? initialActivePath : entrypoint,
  );
  const [view, setView] = useState<StudioView>(initialView);
  const [selections, setSelections] = useState<
    Record<string, ArtifactEditorSelection>
  >(() => ({ ...initialSelections }));
  const [status, setStatus] = useState("Compiler starting");
  const [error, setError] = useState<string>();
  const [accepted, setAccepted] = useState<AcceptedArtifact>();
  const [previewRuntime, setPreviewRuntime] = useState("idle");
  const [replayTarget, setReplayTarget] = useState<number>();
  const [runtimeDiagnostic, setRuntimeDiagnostic] = useState<
    RuntimeDiagnostic
  >();
  const [revealRequest, setRevealRequest] = useState(0);
  const [previewBoot, setPreviewBoot] = useState(0);
  const [languageStatus, setLanguageStatus] = useState<
    "idle" | "checking" | "ready" | "failed"
  >(languageServiceForPath ? "idle" : "failed");
  const [trace, setTrace] = useState<TraceEntry[]>([]);
  const [historyRestoreStatus, setHistoryRestoreStatus] = useState<
    "idle" | "loading" | "failed"
  >("idle");
  const [historyRestoreError, setHistoryRestoreError] = useState<string>();
  const compiler = useRef<GenUiCompiler | null>(null);
  const performanceTracker = useRef<GenUiPerformanceTracker | null>(null);
  performanceTracker.current ??= new GenUiPerformanceTracker();
  const historyController = useRef<AbortController | null>(null);
  const filesRef = useRef(files);
  filesRef.current = files;
  const latestEditStartedAt = useRef(performance.now());
  const previewFrame = useRef<HTMLIFrameElement | null>(null);
  const previewChannel = useRef(crypto.randomUUID()).current;
  const revision = useRef(initialRevision);
  const previewUrl = new URL("./genui/preview.html", document.baseURI);
  previewUrl.hash = previewChannel;
  const source = files[activePath] ?? "";
  const baselineSource = baselineFiles[activePath] ?? "";
  const languageService = useMemo(
    () => languageServiceForPath?.(activePath),
    [activePath, languageServiceForPath],
  );

  useEffect(() => {
    compiler.current = new GenUiCompiler();
    return () => compiler.current?.dispose();
  }, []);

  useEffect(() => {
    const tracker = performanceTracker.current;
    if (!tracker) return;
    const diagnostics = () => tracker.snapshot();
    globalThis.window.__hyperTermGenUiDiagnostics = diagnostics;
    let observer: PerformanceObserver | undefined;
    if (
      typeof PerformanceObserver !== "undefined" &&
      PerformanceObserver.supportedEntryTypes.includes("longtask")
    ) {
      observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          tracker.recordLongTask(entry.startTime, entry.duration);
        }
      });
      observer.observe({ entryTypes: ["longtask"] });
    }
    return () => {
      observer?.disconnect();
      if (globalThis.window.__hyperTermGenUiDiagnostics === diagnostics) {
        delete globalThis.window.__hyperTermGenUiDiagnostics;
      }
    };
  }, []);

  useEffect(() => () => historyController.current?.abort(), []);

  useEffect(() => {
    function receivePreviewEvent(event: MessageEvent) {
      if (event.source !== previewFrame.current?.contentWindow) return;
      const message = parsePreviewMessage(event.data, previewChannel);
      if (!message) return;
      if (message.type === "hyper_term_preview_boot") {
        setPreviewRuntime("booting runtime");
      } else if (message.type === "hyper_term_preview_ready") {
        if (
          accepted &&
          (message.artifact_id !== accepted.artifact_id ||
            message.source_revision !== accepted.source_revision)
        ) return;
        performanceTracker.current?.previewReady(
          message.source_revision,
          performance.now(),
        );
        setRuntimeDiagnostic(undefined);
        setReplayTarget(
          message.replay && Number.isSafeInteger(message.target_event_sequence)
            ? message.target_event_sequence
            : undefined,
        );
        setPreviewRuntime(
          message.replay && Number.isSafeInteger(message.target_event_sequence)
            ? `replay #${message.target_event_sequence} · effects substituted`
            : "ready",
        );
      } else if (message.type === "hyper_term_preview_error") {
        if (
          !accepted || message.artifact_id !== accepted.artifact_id ||
          message.source_revision !== accepted.source_revision
        ) return;
        const diagnostic = mapPreviewRuntimeError(
          message,
          accepted.source_map,
        );
        setRuntimeDiagnostic(diagnostic);
        if (
          diagnostic.original &&
          diagnostic.original.file in filesRef.current
        ) {
          setActivePath(diagnostic.original.file);
        }
        setPreviewRuntime(
          diagnostic.original
            ? `runtime error · ${diagnostic.original.file}:${diagnostic.original.line}:${diagnostic.original.column}`
            : "runtime error",
        );
        setView("code");
        setTrace((entries) =>
          [{
            revision: accepted.source_revision,
            label: "Preview runtime failed",
            detail: diagnostic.original
              ? `${diagnostic.original.file}:${diagnostic.original.line}:${diagnostic.original.column} · ${diagnostic.message}`
              : diagnostic.message,
            state: "failed" as const,
          }, ...entries].slice(0, 8)
        );
      } else if (message.type === "hyper_term_preview_trace") {
        if (
          !accepted || message.artifact_id !== accepted.artifact_id ||
          message.source_revision !== accepted.source_revision
        ) return;
        const runtimeEvent = message.event;
        const isAcceptedSource = sameFiles(filesRef.current, baselineFiles);
        const durableRuntimeTrace = isAcceptedSource && Boolean(onRuntimeTrace);
        setTrace((entries) =>
          [{
            revision: accepted.source_revision,
            label: `${runtimeEvent.kind} · ${runtimeEvent.name}`,
            detail: durableRuntimeTrace
              ? "forwarded to Rust evidence journal"
              : isAcceptedSource
              ? "demo broker only · not durable evidence"
              : "local draft only · not durable evidence",
            state: "accepted" as const,
          }, ...entries].slice(0, 8)
        );
        if (durableRuntimeTrace) onRuntimeTrace?.(runtimeEvent);
      }
    }
    globalThis.addEventListener("message", receivePreviewEvent);
    return () => globalThis.removeEventListener("message", receivePreviewEvent);
  }, [accepted, baselineFiles, onRuntimeTrace, previewChannel]);

  useEffect(() => {
    if (!accepted || previewBoot === 0) return;
    setPreviewRuntime("loading artifact");
    previewFrame.current?.contentWindow?.postMessage({
      type: "hyper_term_render_artifact",
      schema_version: 1,
      channel_token: previewChannel,
      artifact: {
        artifact_id: accepted.artifact_id,
        source_revision: accepted.source_revision,
        content_digest: accepted.content_digest,
        bundle: accepted.bundle,
        css: accepted.css,
        source_map: accepted.source_map,
      },
    }, "*");
  }, [accepted, previewBoot, previewChannel]);

  useEffect(() => {
    setRuntimeDiagnostic(undefined);
    const editStartedAt = latestEditStartedAt.current;
    const sourceRevision = ++revision.current;
    const timer = globalThis.setTimeout(async () => {
      const activeCompiler = compiler.current;
      if (!activeCompiler) return;
      performanceTracker.current?.begin(
        sourceRevision,
        editStartedAt,
        performance.now(),
      );
      setStatus(`Local build r${sourceRevision}`);
      setError(undefined);
      setTrace((entries) =>
        [{
          revision: sourceRevision,
          label: "Compile candidate",
          detail: `${
            Object.keys(files).length
          } bounded file(s) → esbuild-wasm Worker`,
          state: "working" as const,
        }, ...entries].slice(0, 8)
      );
      try {
        const candidate = await activeCompiler.compile(
          sourceRevision,
          entrypoint,
          files,
        );
        performanceTracker.current?.candidateReady(
          sourceRevision,
          performance.now(),
        );
        const nextAccepted = await host.acceptArtifact(candidate);
        performanceTracker.current?.accepted(sourceRevision, performance.now());
        if (sourceRevision !== revision.current) {
          performanceTracker.current?.cancel(sourceRevision);
          return;
        }
        setAccepted(nextAccepted);
        setStatus(
          `Preview ready r${sourceRevision} · ${candidate.bundle.length} B`,
        );
        setTrace((entries) =>
          [{
            revision: sourceRevision,
            label: "Local preview accepted",
            detail: `${nextAccepted.artifact_id} · browser sandbox`,
            state: "accepted" as const,
          }, ...entries.filter((entry) => entry.revision !== sourceRevision)]
            .slice(0, 8)
        );
      } catch (compileError) {
        performanceTracker.current?.cancel(sourceRevision);
        if (sourceRevision !== revision.current) return;
        const message = compileError instanceof Error
          ? compileError.message
          : String(compileError);
        setError(message);
        setStatus(`Rejected r${sourceRevision} · last good kept`);
        setTrace((entries) =>
          [{
            revision: sourceRevision,
            label: "Candidate rejected",
            detail: message,
            state: "failed" as const,
          }, ...entries.filter((entry) => entry.revision !== sourceRevision)]
            .slice(0, 8)
        );
      }
    }, LIVE_BUILD_SETTLE_MS);
    return () => {
      clearTimeout(timer);
      performanceTracker.current?.cancel(sourceRevision);
    };
  }, [entrypoint, files, host]);

  const runtimeLocation = runtimeDiagnostic?.original;
  const changedPaths = Object.keys(files).filter((path) =>
    files[path] !== baselineFiles[path]
  );
  const draftChanged = changedPaths.length > 0;
  const publishBusy = publishStatus === "waiting_approval" ||
    publishStatus === "compiling";

  const loadHistoryAsDraft = async (entry: ArtifactHistoryEntry) => {
    if (!onLoadHistorySource || publishBusy) return;
    historyController.current?.abort();
    const controller = new AbortController();
    historyController.current = controller;
    setHistoryRestoreStatus("loading");
    setHistoryRestoreError(undefined);
    try {
      const historical = await onLoadHistorySource(entry, controller.signal);
      if (controller.signal.aborted) return;
      const currentPaths = Object.keys(baselineFiles).sort();
      const historicalPaths = Object.keys(historical.files).sort();
      if (
        historical.entrypoint !== entrypoint ||
        currentPaths.length !== historicalPaths.length ||
        currentPaths.some((path, index) => path !== historicalPaths[index])
      ) {
        throw new Error(
          "This revision uses a different virtual file tree and cannot be restored into the current fixed-path draft.",
        );
      }
      latestEditStartedAt.current = performance.now();
      setFiles({ ...historical.files });
      setSelections((current) =>
        normalizeSelections(current, historical.files)
      );
      if (!(activePath in historical.files)) {
        setActivePath(historical.entrypoint);
      }
      setHistoryRestoreStatus("idle");
      setTrace((entries) =>
        [{
          revision: historical.source_revision,
          label: "Historical source loaded as draft",
          detail: `journal #${entry.event_sequence} · no effects replayed`,
          state: "accepted" as const,
        }, ...entries].slice(0, 8)
      );
      setView("diff");
    } catch (historyError) {
      if (controller.signal.aborted) return;
      setHistoryRestoreStatus("failed");
      setHistoryRestoreError(
        historyError instanceof Error
          ? historyError.message
          : String(historyError),
      );
    }
  };

  useEffect(() => {
    onDraftStateChange?.(draftChanged);
  }, [draftChanged, onDraftStateChange]);

  useEffect(() => {
    onActivePathChange?.(activePath);
    setLanguageStatus(languageService ? "idle" : "failed");
  }, [activePath, languageService, onActivePathChange]);

  useEffect(() => {
    onCheckpointStateChange?.({
      files: { ...files },
      activePath,
      view,
      selections: { ...selections },
    });
  }, [activePath, files, onCheckpointStateChange, selections, view]);

  const updateSource = (nextSource: string) => {
    if (filesRef.current[activePath] === nextSource) return;
    latestEditStartedAt.current = performance.now();
    setFiles((current) => ({ ...current, [activePath]: nextSource }));
    setSelections((current) =>
      normalizeSelections(current, {
        ...filesRef.current,
        [activePath]: nextSource,
      })
    );
  };

  const replayRuntimeTrace = (target: RuntimeTraceEvent) => {
    if (
      !accepted || !isReplayBoundary(target) ||
      !/^[0-9a-f]{64}$/.test(runtimeTraceProjectionDigest) ||
      !sameFiles(filesRef.current, baselineFiles)
    ) return;
    setRuntimeDiagnostic(undefined);
    setReplayTarget(target.event_sequence);
    setPreviewRuntime(`verifying replay #${target.event_sequence}`);
    previewFrame.current?.contentWindow?.postMessage({
      type: "hyper_term_replay_artifact",
      schema_version: 1,
      channel_token: previewChannel,
      artifact: {
        artifact_id: accepted.artifact_id,
        source_revision: accepted.source_revision,
        content_digest: accepted.content_digest,
        bundle: accepted.bundle,
        css: accepted.css,
        source_map: accepted.source_map,
      },
      replay: {
        source_revision: initialRevision,
        target_event_sequence: target.event_sequence,
        projection_digest: runtimeTraceProjectionDigest,
        events: runtimeTraceEvents,
      },
    }, "*");
  };

  const returnToLivePreview = () => {
    if (!accepted) return;
    setReplayTarget(undefined);
    setRuntimeDiagnostic(undefined);
    setPreviewRuntime("returning to live preview");
    previewFrame.current?.contentWindow?.postMessage({
      type: "hyper_term_render_artifact",
      schema_version: 1,
      channel_token: previewChannel,
      artifact: {
        artifact_id: accepted.artifact_id,
        source_revision: accepted.source_revision,
        content_digest: accepted.content_digest,
        bundle: accepted.bundle,
        css: accepted.css,
        source_map: accepted.source_map,
      },
    }, "*");
  };

  return (
    <aside className="studio" aria-label="Agentic UI Studio">
      <header className="studio-header">
        <div>
          <span className="eyebrow">Agentic UI Studio</span>
          <h2>{heading ?? activePath}</h2>
        </div>
        <div className="studio-actions">
          <span
            className={`checkpoint-status ${checkpointStatus}`}
            title={checkpointError ??
              "Unpublished editor state is versioned by the Rust checkpoint journal"}
            role={checkpointStatus === "failed" ? "alert" : "status"}
          >
            Draft sync · {checkpointStatusLabel(checkpointStatus)}
          </span>
          <span className={`compiler-status ${error ? "has-error" : ""}`}>
            <span /> {status}
          </span>
          {onPublishDraft && (
            <button
              className="publish-draft"
              data-state={publishStatus}
              type="button"
              disabled={!draftChanged || publishBusy || Boolean(error)}
              title={publishError ??
                "Create an Approval Block, then rebuild this draft with Rust-supervised Deno"}
              onClick={() => onPublishDraft({ ...files })}
            >
              {publishDraftLabel(
                publishStatus,
                draftChanged,
                initialRevision,
              )}
            </button>
          )}
        </div>
      </header>
      <ArtifactFileTabs
        activePath={activePath}
        baselineFiles={baselineFiles}
        entrypoint={entrypoint}
        files={files}
        onSelect={setActivePath}
      />
      <div className="studio-tabs" role="tablist" aria-label="Artifact tools">
        {(["code", "diff", "trace"] as const).map((tab) => (
          <button
            key={tab}
            className={view === tab ? "active" : ""}
            type="button"
            role="tab"
            aria-selected={view === tab}
            onClick={() => setView(tab)}
          >
            {tab === "code" ? "Code" : tab === "diff" ? "Diff" : "Time Travel"}
          </button>
        ))}
        <span className="studio-spacer" />
        {languageService && (
          <span
            className={`language-status ${languageStatus}`}
            title="Rust-supervised Deno LSP against the private artifact snapshot"
          >
            Deno LSP · {languageStatus}
          </span>
        )}
        <span className="source-revision">source r{revision.current || 1}</span>
      </div>
      <div className="studio-editor">
        {view === "code" && (
          <CodeEditor
            value={source}
            documentPath={activePath}
            draftFiles={files}
            onChange={updateSource}
            readOnly={publishBusy}
            revealLocation={runtimeLocation?.file === activePath
              ? runtimeLocation
              : undefined}
            revealRequest={revealRequest}
            languageService={languageService}
            onLanguageStatus={setLanguageStatus}
            selection={selections[activePath]}
            onSelectionChange={(selection) =>
              setSelections((current) =>
                current[activePath]?.anchor === selection.anchor &&
                  current[activePath]?.head === selection.head
                  ? current
                  : { ...current, [activePath]: selection }
              )}
          />
        )}
        {view === "diff" && (
          <CodeDiff
            original={baselineSource}
            modified={source}
            onChange={updateSource}
            readOnlyModified={publishBusy}
          />
        )}
        {view === "trace" && (
          <TraceTimeline
            entries={trace}
            historyEntries={historyEntries}
            historyStatus={historyStatus}
            historyError={historyRestoreError ?? historyError}
            historyRestoreStatus={historyRestoreStatus}
            publishBusy={publishBusy}
            draftChanged={draftChanged}
            onLoadHistory={onLoadHistorySource
              ? (entry) => void loadHistoryAsDraft(entry)
              : undefined}
            runtimeTraceEvents={runtimeTraceEvents}
            runtimeTraceProjectionDigest={runtimeTraceProjectionDigest}
            runtimeTraceStatus={runtimeTraceStatus}
            runtimeTraceError={runtimeTraceError}
            replayDisabled={draftChanged || publishBusy || !accepted}
            onReplayRuntimeTrace={replayRuntimeTrace}
            onPrepareBugCapsule={onPrepareBugCapsule}
          />
        )}
      </div>
      {error
        ? <div className="compile-error" role="alert">{error}</div>
        : publishError
        ? (
          <div className="compile-error publish-error" role="alert">
            {publishError}
          </div>
        )
        : runtimeDiagnostic && (
          <div
            className="compile-error runtime-error"
            role="alert"
            title={runtimeDiagnostic.stack}
          >
            <button
              type="button"
              onClick={() => {
                setView("code");
                setRevealRequest((request) => request + 1);
              }}
            >
              <strong>
                Runtime{runtimeLocation
                  ? ` · ${runtimeLocation.file}:${runtimeLocation.line}:${runtimeLocation.column}`
                  : ""}
              </strong>
              <span>{runtimeDiagnostic.message}</span>
            </button>
          </div>
        )}
      <div className="preview-header">
        <div>
          <span className="eyebrow">Isolated local preview</span>
          <strong>
            {accepted?.artifact_id ?? "Waiting for accepted artifact"}
          </strong>
        </div>
        <div className="preview-badges">
          {replayTarget !== undefined && (
            <button type="button" onClick={returnToLivePreview}>
              Return to live
            </button>
          )}
          <span
            className={previewRuntime.startsWith("runtime error") ? "bad" : ""}
          >
            {previewRuntime}
          </span>
          <span>no network</span>
          <span>sandbox</span>
          <span>{host.authority}</span>
        </div>
      </div>
      <div className="preview-frame">
        <iframe
          ref={previewFrame}
          title="Accepted Agentic UI artifact"
          sandbox="allow-scripts"
          src={previewUrl.href}
          onLoad={() => {
            setPreviewRuntime("booting runtime");
            setPreviewBoot((generation) => generation + 1);
          }}
        />
      </div>
    </aside>
  );
}

function publishDraftLabel(
  status: ArtifactDraftStatus | "idle",
  changed: boolean,
  revision: number,
): string {
  if (status === "waiting_approval") return "Approve publish";
  if (status === "compiling") return "Deno compiling";
  if (status === "accepted" && !changed) return `Published r${revision}`;
  if (status === "failed" || status === "rejected") return "Retry publish";
  return "Publish draft";
}

function checkpointStatusLabel(
  status: "restored" | "idle" | "saving" | "saved" | "failed",
): string {
  switch (status) {
    case "restored":
      return "restored";
    case "saving":
      return "saving";
    case "saved":
      return "saved";
    case "failed":
      return "attention";
    case "idle":
      return "ready";
  }
}

function normalizeSelections(
  selections: Record<string, ArtifactEditorSelection>,
  files: Record<string, string>,
): Record<string, ArtifactEditorSelection> {
  return Object.fromEntries(
    Object.entries(selections)
      .filter(([path]) => path in files)
      .map(([path, selection]) => {
        const maximum = files[path].length;
        return [path, {
          anchor: Math.min(selection.anchor, maximum),
          head: Math.min(selection.head, maximum),
        }];
      }),
  );
}

interface TraceTimelineProps {
  entries: TraceEntry[];
  historyEntries: ArtifactHistoryEntry[];
  historyStatus: "loading" | "ready" | "failed";
  historyError?: string;
  historyRestoreStatus: "idle" | "loading" | "failed";
  publishBusy: boolean;
  draftChanged: boolean;
  onLoadHistory?: (entry: ArtifactHistoryEntry) => void;
  runtimeTraceEvents: RuntimeTraceEvent[];
  runtimeTraceProjectionDigest: string;
  runtimeTraceStatus: "loading" | "ready" | "saving" | "failed";
  runtimeTraceError?: string;
  replayDisabled: boolean;
  onReplayRuntimeTrace: (event: RuntimeTraceEvent) => void;
  onPrepareBugCapsule?: () => Promise<BugCapsule>;
}

function TraceTimeline({
  entries,
  historyEntries,
  historyStatus,
  historyError,
  historyRestoreStatus,
  publishBusy,
  draftChanged,
  onLoadHistory,
  runtimeTraceEvents,
  runtimeTraceProjectionDigest,
  runtimeTraceStatus,
  runtimeTraceError,
  replayDisabled,
  onReplayRuntimeTrace,
  onPrepareBugCapsule,
}: TraceTimelineProps) {
  const [capsule, setCapsule] = useState<BugCapsule>();
  const [capsuleStatus, setCapsuleStatus] = useState<
    "idle" | "preparing" | "ready" | "failed"
  >("idle");
  const [capsuleError, setCapsuleError] = useState<string>();

  const prepareCapsule = () => {
    if (!onPrepareBugCapsule || capsuleStatus === "preparing") return;
    setCapsuleStatus("preparing");
    setCapsuleError(undefined);
    onPrepareBugCapsule().then((prepared) => {
      setCapsule(prepared);
      setCapsuleStatus("ready");
    }).catch((error: unknown) => {
      setCapsuleStatus("failed");
      setCapsuleError(error instanceof Error ? error.message : String(error));
    });
  };

  return (
    <div className="trace-timeline">
      <div className="trace-capsule-toolbar">
        <div>
          <strong>Offline Bug Capsule</strong>
          <span>Rust-redacted · replay only · source digest only</span>
        </div>
        <button
          type="button"
          disabled={!onPrepareBugCapsule || capsuleStatus === "preparing"}
          onClick={capsule ? () => downloadBugCapsule(capsule) : prepareCapsule}
        >
          {capsule
            ? "Download JSON"
            : capsuleStatus === "preparing"
            ? "Preparing…"
            : "Preview export"}
        </button>
      </div>
      {capsuleError && (
        <p className="trace-history-error" role="alert">{capsuleError}</p>
      )}
      {capsule && (
        <details className="trace-capsule-inventory" open>
          <summary>
            Exact export inventory · {capsule.capsule_digest.slice(0, 10)}
          </summary>
          {capsule.inventory.map((entry) => (
            <div key={entry.category} data-inclusion={entry.inclusion}>
              <strong>{entry.category.replaceAll("_", " ")}</strong>
              <span>{entry.inclusion.replaceAll("_", " ")}</span>
              <small>
                {entry.item_count} item(s) · {formatByteCount(entry.byte_count)}
              </small>
              <p>{entry.reason}</p>
            </div>
          ))}
        </details>
      )}
      <div className="trace-section-heading">
        <strong>Runtime checkpoints</strong>
        <span>{runtimeTraceStatus}</span>
      </div>
      {runtimeTraceError && (
        <p className="trace-history-error" role="alert">
          {runtimeTraceError}
        </p>
      )}
      {runtimeTraceEvents.length === 0 && runtimeTraceStatus === "ready" && (
        <p className="dim">
          No accepted-source actions or checkpoints recorded yet.
        </p>
      )}
      {[...runtimeTraceEvents].reverse().map((event) => (
        <div
          className="trace-entry runtime"
          data-state="accepted"
          key={event.event_sequence}
        >
          <span className="trace-node" />
          <div>
            <strong>{event.kind} · {event.name}</strong>
            <p>
              event #{event.event_sequence} · {formatHistoryTime(
                event.recorded_at_ms,
              )} · {event.payload_digest.slice(0, 10)}
              {event.redacted ? " · redacted" : ""}
            </p>
          </div>
          {isReplayBoundary(event) && (
            <button
              type="button"
              disabled={replayDisabled || runtimeTraceStatus !== "ready" ||
                !runtimeTraceProjectionDigest ||
                (event.kind === "effect_receipt" && event.redacted)}
              title={event.redacted
                ? "Redacted receipts cannot be substituted"
                : "Rebuild reducer state through this event without invoking live effects"}
              onClick={() => onReplayRuntimeTrace(event)}
            >
              Replay to here
            </button>
          )}
        </div>
      ))}
      <div className="trace-section-heading">
        <strong>Accepted source</strong>
        <span>{historyStatus}</span>
      </div>
      {historyStatus === "loading" && (
        <p className="dim">Loading durable Artifact history…</p>
      )}
      {historyError && (
        <p className="trace-history-error" role="alert">{historyError}</p>
      )}
      {historyEntries.map((entry, index) => (
        <div
          className="trace-entry durable"
          data-state="accepted"
          key={entry.artifact.artifact_id}
        >
          <span className="trace-node" />
          <div>
            <strong>
              source r{entry.artifact.source_revision}
              {index === 0 ? " · Current" : " · Accepted"}
            </strong>
            <p>
              journal #{entry.event_sequence} · {formatHistoryTime(
                entry.recorded_at_ms,
              )} · {entry.artifact.content_digest.slice(0, 10)}
            </p>
          </div>
          {onLoadHistory && (
            <button
              type="button"
              disabled={publishBusy || historyRestoreStatus === "loading" ||
                index === 0 && !draftChanged}
              title={index === 0
                ? "Discard local edits and reload the current Rust revision"
                : "Load this historical source as a local draft for Diff and preview"}
              onClick={() => onLoadHistory(entry)}
            >
              {index === 0 ? "Reset draft" : "Load as draft"}
            </button>
          )}
        </div>
      ))}
      {historyEntries.length === 0 && historyStatus === "ready" &&
        entries.length === 0 && (
        <p className="dim">Compile once to create a trace.</p>
      )}
      {entries.map((entry, index) => (
        <div
          className="trace-entry"
          data-state={entry.state}
          key={`${entry.revision}-${index}`}
        >
          <span className="trace-node" />
          <div>
            <strong>r{entry.revision} · {entry.label}</strong>
            <p>{entry.detail}</p>
          </div>
        </div>
      ))}
    </div>
  );
}

function sameFiles(
  left: Record<string, string>,
  right: Record<string, string>,
): boolean {
  const leftPaths = Object.keys(left).sort();
  const rightPaths = Object.keys(right).sort();
  return leftPaths.length === rightPaths.length &&
    leftPaths.every((path, index) =>
      path === rightPaths[index] && left[path] === right[path]
    );
}

function formatHistoryTime(recordedAtMs: number): string {
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(recordedAtMs));
}

function formatByteCount(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}
