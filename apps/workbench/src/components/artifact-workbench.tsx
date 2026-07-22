import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArtifactDraftPublisher,
  type ArtifactDraftStatus,
} from "../artifact-draft-publisher.ts";
import {
  ArtifactHistoryClient,
  type ArtifactHistoryEntry,
  type ArtifactHistorySource,
} from "../artifact-history-client.ts";
import {
  type ArtifactEditorCheckpoint,
  ArtifactEditorCheckpointClient,
  type ArtifactEditorCheckpointInput,
} from "../artifact-editor-checkpoint.ts";
import { resolveHost } from "../host.ts";
import { ArtifactLanguageService } from "../editor-language-service.ts";
import {
  RuntimeTraceClient,
  type RuntimeTraceEvent,
  type RuntimeTraceInput,
  type RuntimeTraceProjection,
} from "../runtime-trace-client.ts";
import {
  type WorkspaceApplyMapping,
  type WorkspaceApplyPreview,
  WorkspaceApplyPublisher,
  type WorkspaceApplyStatus,
  type WorkspaceApplyUpdate,
  type WorkspaceHunkSelection,
} from "../workspace-apply-publisher.ts";
import { BugCapsuleClient } from "../debug-capsule-client.ts";
import { GenUiStudio } from "./genui-studio.tsx";
import { VisualQualityGate } from "./visual-quality-gate.tsx";
import { WorkspaceReview } from "./workspace-review.tsx";
import { WorkspaceApplyComposer } from "./workspace-apply-composer.tsx";

interface ArtifactSourceResponse {
  artifact_id: string;
  source_revision: number;
  entrypoint: string;
  files: Record<string, string>;
}

type LoadState =
  | { kind: "loading" }
  | { kind: "failed"; message: string }
  | {
    kind: "ready";
    source: ArtifactSourceResponse;
    checkpoint: ArtifactEditorCheckpoint;
  };

type HistoryState =
  | { kind: "loading" }
  | { kind: "failed"; message: string }
  | {
    kind: "ready";
    activeArtifactId: string;
    entries: ArtifactHistoryEntry[];
  };

interface ArtifactContext {
  artifactId: string;
  sessionId: number;
  token: string;
}

export function ArtifactWorkbench() {
  const host = useMemo(resolveHost, []);
  const context = useMemo(readArtifactContext, []);
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [historyState, setHistoryState] = useState<HistoryState>({
    kind: "loading",
  });
  const [publishStatus, setPublishStatus] = useState<
    ArtifactDraftStatus | "idle"
  >("idle");
  const [publishError, setPublishError] = useState<string>();
  const [hasLocalDraft, setHasLocalDraft] = useState(false);
  const [checkpointStatus, setCheckpointStatus] = useState<
    "restored" | "idle" | "saving" | "saved" | "failed"
  >("idle");
  const [checkpointError, setCheckpointError] = useState<string>();
  const [pendingCheckpoint, setPendingCheckpoint] = useState<
    ArtifactEditorCheckpointInput
  >();
  const [applyPreview, setApplyPreview] = useState<WorkspaceApplyPreview>();
  const [applyReview, setApplyReview] = useState<WorkspaceApplyUpdate>();
  const [applyError, setApplyError] = useState<string>();
  const [workspacePanel, setWorkspacePanel] = useState<
    "none" | "mapping" | "review"
  >("none");
  const [previewStarting, setPreviewStarting] = useState(false);
  const [applyStarting, setApplyStarting] = useState(false);
  const [runtimeTraceEvents, setRuntimeTraceEvents] = useState<
    RuntimeTraceEvent[]
  >([]);
  const [runtimeTraceProjectionDigest, setRuntimeTraceProjectionDigest] =
    useState("");
  const [runtimeTraceStatus, setRuntimeTraceStatus] = useState<
    "loading" | "ready" | "saving" | "failed"
  >("loading");
  const [runtimeTraceError, setRuntimeTraceError] = useState<string>();
  const publishController = useRef<AbortController | undefined>(undefined);
  const applyController = useRef<AbortController | undefined>(undefined);
  const checkpointRevision = useRef(0);
  const checkpointGeneration = useRef(0);
  const lastCheckpointFingerprint = useRef("");
  const checkpointQueue = useRef<Promise<void>>(Promise.resolve());
  const checkpointControllers = useRef(new Set<AbortController>());
  const runtimeTraceQueue = useRef<RuntimeTraceInput[]>([]);
  const runtimeTraceTimer = useRef<number | undefined>(undefined);
  const runtimeTraceAppendQueue = useRef<Promise<void>>(Promise.resolve());
  const runtimeTraceGeneration = useRef(0);
  const runtimeTraceFailed = useRef(false);
  const runtimeTraceClient = useMemo(() => {
    if (!context || state.kind !== "ready") return undefined;
    return new RuntimeTraceClient({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      sessionId: context.sessionId,
      token: context.token,
    });
  }, [context, state]);
  const languageServiceForPath = useCallback((documentPath: string) => {
    if (!context || state.kind !== "ready") {
      throw new Error("Artifact language service context is unavailable.");
    }
    return new ArtifactLanguageService({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      documentPath,
      sessionId: context.sessionId,
      token: context.token,
    });
  }, [context, state]);
  const prepareBugCapsule = useCallback(() => {
    if (!context || state.kind !== "ready") {
      return Promise.reject(new Error("Bug Capsule context is unavailable."));
    }
    return new BugCapsuleClient({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      sessionId: context.sessionId,
      token: context.token,
    }).prepare();
  }, [context, state]);

  useEffect(() => {
    if (!context) {
      setState({
        kind: "failed",
        message: "Artifact editor context is invalid.",
      });
      return;
    }
    const controller = new AbortController();
    fetchArtifactWorkspace(context, context.artifactId, controller.signal)
      .then(({ source, checkpoint }) => {
        checkpointGeneration.current += 1;
        checkpointRevision.current = checkpoint.revision;
        lastCheckpointFingerprint.current = checkpointFingerprint({
          files: checkpoint.files,
          activePath: checkpoint.active_path,
          view: checkpoint.view,
          selections: checkpoint.selections,
        });
        setCheckpointStatus(checkpoint.revision > 0 ? "restored" : "idle");
        setCheckpointError(undefined);
        setHasLocalDraft(!sameFiles(checkpoint.files, source.files));
        setState({ kind: "ready", source, checkpoint });
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setState({
          kind: "failed",
          message: error instanceof Error ? error.message : String(error),
        });
      });
    return () => controller.abort();
  }, [context]);

  useEffect(() => {
    if (!context || state.kind !== "ready") return;
    const controller = new AbortController();
    const activeArtifactId = state.source.artifact_id;
    setHistoryState({ kind: "loading" });
    new ArtifactHistoryClient({
      activeArtifactId,
      sessionId: context.sessionId,
      token: context.token,
    }).list(controller.signal).then((entries) => {
      if (controller.signal.aborted) return;
      setHistoryState({ kind: "ready", activeArtifactId, entries });
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setHistoryState({
        kind: "failed",
        message: error instanceof Error ? error.message : String(error),
      });
    });
    return () => controller.abort();
  }, [context, state]);

  useEffect(() => {
    if (!runtimeTraceClient) return;
    const generation = ++runtimeTraceGeneration.current;
    const controller = new AbortController();
    runtimeTraceQueue.current = [];
    runtimeTraceFailed.current = false;
    runtimeTraceAppendQueue.current = Promise.resolve();
    if (runtimeTraceTimer.current !== undefined) {
      clearTimeout(runtimeTraceTimer.current);
      runtimeTraceTimer.current = undefined;
    }
    setRuntimeTraceStatus("loading");
    setRuntimeTraceError(undefined);
    runtimeTraceClient.list(controller.signal).then((projection) => {
      if (
        controller.signal.aborted ||
        generation !== runtimeTraceGeneration.current
      ) return;
      setRuntimeTraceEvents(projection.events);
      setRuntimeTraceProjectionDigest(projection.projection_digest);
      setRuntimeTraceStatus("ready");
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      runtimeTraceFailed.current = true;
      setRuntimeTraceStatus("failed");
      setRuntimeTraceError(
        error instanceof Error ? error.message : String(error),
      );
    });
    return () => {
      controller.abort();
      runtimeTraceGeneration.current += 1;
      runtimeTraceQueue.current = [];
      if (runtimeTraceTimer.current !== undefined) {
        clearTimeout(runtimeTraceTimer.current);
        runtimeTraceTimer.current = undefined;
      }
    };
  }, [runtimeTraceClient]);

  const flushRuntimeTrace = useCallback(() => {
    if (runtimeTraceTimer.current !== undefined) {
      clearTimeout(runtimeTraceTimer.current);
      runtimeTraceTimer.current = undefined;
    }
    if (!runtimeTraceClient || runtimeTraceFailed.current) return;
    const batch = runtimeTraceQueue.current.splice(0, 16);
    if (batch.length === 0) return;
    const generation = runtimeTraceGeneration.current;
    runtimeTraceAppendQueue.current = runtimeTraceAppendQueue.current
      .catch(() => {})
      .then(async () => {
        if (
          generation !== runtimeTraceGeneration.current ||
          runtimeTraceFailed.current
        ) return;
        setRuntimeTraceStatus("saving");
        setRuntimeTraceError(undefined);
        try {
          const projection = await appendRuntimeTraceWithRetry(
            runtimeTraceClient,
            batch,
          );
          if (generation !== runtimeTraceGeneration.current) return;
          setRuntimeTraceEvents(projection.events);
          setRuntimeTraceProjectionDigest(projection.projection_digest);
          setRuntimeTraceStatus("ready");
        } catch (error) {
          if (generation !== runtimeTraceGeneration.current) return;
          runtimeTraceFailed.current = true;
          runtimeTraceQueue.current = [];
          setRuntimeTraceStatus("failed");
          setRuntimeTraceError(
            error instanceof Error ? error.message : String(error),
          );
        }
      });
  }, [runtimeTraceClient]);

  const recordRuntimeTrace = useCallback((event: RuntimeTraceInput) => {
    if (!runtimeTraceClient || runtimeTraceFailed.current) return;
    runtimeTraceQueue.current.push(event);
    if (runtimeTraceQueue.current.length >= 16) {
      flushRuntimeTrace();
      return;
    }
    if (runtimeTraceTimer.current === undefined) {
      runtimeTraceTimer.current = globalThis.setTimeout(
        flushRuntimeTrace,
        48,
      );
    }
  }, [flushRuntimeTrace, runtimeTraceClient]);

  useEffect(() => () => {
    publishController.current?.abort();
    applyController.current?.abort();
    for (const controller of checkpointControllers.current) {
      controller.abort();
    }
    checkpointControllers.current.clear();
  }, []);

  useEffect(() => {
    if (!context || state.kind !== "ready" || !pendingCheckpoint) return;
    const fingerprint = checkpointFingerprint(pendingCheckpoint);
    if (fingerprint === lastCheckpointFingerprint.current) return;
    const generation = checkpointGeneration.current;
    const timer = globalThis.setTimeout(() => {
      const input = cloneCheckpointInput(pendingCheckpoint);
      checkpointQueue.current = checkpointQueue.current.catch(() => {}).then(
        async () => {
          if (generation !== checkpointGeneration.current) return;
          const nextFingerprint = checkpointFingerprint(input);
          if (nextFingerprint === lastCheckpointFingerprint.current) return;
          const controller = new AbortController();
          checkpointControllers.current.add(controller);
          setCheckpointStatus("saving");
          setCheckpointError(undefined);
          try {
            const saved = await new ArtifactEditorCheckpointClient({
              artifactId: state.source.artifact_id,
              sourceRevision: state.source.source_revision,
              entrypoint: state.source.entrypoint,
              files: state.source.files,
              sessionId: context.sessionId,
              token: context.token,
            }).save(checkpointRevision.current, input, controller.signal);
            if (generation !== checkpointGeneration.current) return;
            checkpointRevision.current = saved.revision;
            lastCheckpointFingerprint.current = checkpointFingerprint({
              files: saved.files,
              activePath: saved.active_path,
              view: saved.view,
              selections: saved.selections,
            });
            setCheckpointStatus("saved");
          } catch (error) {
            if (controller.signal.aborted) return;
            setCheckpointStatus("failed");
            setCheckpointError(
              error instanceof Error ? error.message : String(error),
            );
          } finally {
            checkpointControllers.current.delete(controller);
          }
        },
      );
    }, 420);
    return () => clearTimeout(timer);
  }, [context, pendingCheckpoint, state]);

  const publishDraft = useCallback((files: Record<string, string>) => {
    if (!context || state.kind !== "ready") return;
    publishController.current?.abort();
    const controller = new AbortController();
    publishController.current = controller;
    setPublishError(undefined);
    const publisher = new ArtifactDraftPublisher({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      entrypoint: state.source.entrypoint,
      files: state.source.files,
      sessionId: context.sessionId,
      token: context.token,
    });
    publisher.publish(
      files,
      (update) => setPublishStatus(update.status),
      controller.signal,
    ).then((artifact) =>
      fetchArtifactWorkspace(context, artifact.artifact_id, controller.signal)
    ).then(({ source: nextSource, checkpoint }) => {
      if (controller.signal.aborted) return;
      checkpointGeneration.current += 1;
      for (const checkpointController of checkpointControllers.current) {
        checkpointController.abort();
      }
      checkpointControllers.current.clear();
      checkpointRevision.current = checkpoint.revision;
      lastCheckpointFingerprint.current = checkpointFingerprint({
        files: checkpoint.files,
        activePath: checkpoint.active_path,
        view: checkpoint.view,
        selections: checkpoint.selections,
      });
      setPendingCheckpoint(undefined);
      setCheckpointStatus("idle");
      setCheckpointError(undefined);
      setState({ kind: "ready", source: nextSource, checkpoint });
      setPublishStatus("accepted");
      setHasLocalDraft(false);
      setApplyPreview(undefined);
      setApplyReview(undefined);
      setWorkspacePanel("none");
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setPublishStatus("failed");
      setPublishError(error instanceof Error ? error.message : String(error));
    });
  }, [context, state]);

  const loadHistorySource = useCallback((
    entry: ArtifactHistoryEntry,
    signal: AbortSignal,
  ): Promise<ArtifactHistorySource> => {
    if (!context || state.kind !== "ready") {
      return Promise.reject(
        new Error("Artifact history context is unavailable."),
      );
    }
    return new ArtifactHistoryClient({
      activeArtifactId: state.source.artifact_id,
      sessionId: context.sessionId,
      token: context.token,
    }).source(entry, signal);
  }, [context, state]);

  const reviewWorkspace = useCallback((mappings: WorkspaceApplyMapping[]) => {
    if (!context || state.kind !== "ready" || hasLocalDraft) return;
    applyController.current?.abort();
    const controller = new AbortController();
    applyController.current = controller;
    setPreviewStarting(true);
    setApplyError(undefined);
    setApplyPreview(undefined);
    setApplyReview(undefined);
    const publisher = new WorkspaceApplyPublisher({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      sessionId: context.sessionId,
      token: context.token,
    });
    publisher.preview(mappings, controller.signal).then((preview) => {
      if (controller.signal.aborted) return;
      setPreviewStarting(false);
      setApplyPreview(preview);
      setWorkspacePanel("review");
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setPreviewStarting(false);
      setApplyError(error instanceof Error ? error.message : String(error));
    });
  }, [context, hasLocalDraft, state]);

  const applyWorkspace = useCallback((
    preview: WorkspaceApplyPreview,
    selections: WorkspaceHunkSelection[],
  ) => {
    if (!context || state.kind !== "ready" || hasLocalDraft) return;
    applyController.current?.abort();
    const controller = new AbortController();
    applyController.current = controller;
    setApplyStarting(true);
    setApplyError(undefined);
    setApplyReview(undefined);
    const publisher = new WorkspaceApplyPublisher({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      sessionId: context.sessionId,
      token: context.token,
    });
    publisher.apply(
      preview,
      selections,
      (update) => {
        setApplyStarting(false);
        setApplyReview(update);
      },
      controller.signal,
    ).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setApplyStarting(false);
      setApplyError(error instanceof Error ? error.message : String(error));
    });
  }, [context, hasLocalDraft, state]);

  const applyStatus: WorkspaceApplyStatus | "idle" = applyReview?.status ??
    "idle";
  const applyBusy = previewStarting || applyStarting ||
    applyStatus === "waiting_approval" || applyStatus === "applying";

  return (
    <main className="artifact-surface">
      <header className="artifact-surface-header">
        <div>
          <span className="eyebrow">Trusted artifact workbench</span>
          <strong>
            {state.kind === "ready"
              ? state.source.entrypoint
              : "Loading source"}
          </strong>
        </div>
        <span>workspace writes require approval</span>
      </header>
      {state.kind === "loading" && (
        <div className="artifact-surface-state" role="status">
          Loading the task-current source snapshot from Rust…
        </div>
      )}
      {state.kind === "failed" && (
        <div className="artifact-surface-state failed" role="alert">
          <strong>Source unavailable</strong>
          <span>{state.message}</span>
        </div>
      )}
      {state.kind === "ready" && (
        <div className="artifact-workbench-stage">
          <div
            className="artifact-editor-layer"
            data-obscured={workspacePanel !== "none" || undefined}
            aria-hidden={workspacePanel !== "none" || undefined}
          >
            <GenUiStudio
              key={`${state.source.artifact_id}:${state.source.source_revision}`}
              host={host}
              entrypoint={state.source.entrypoint}
              initialFiles={state.checkpoint.files}
              baselineFiles={state.source.files}
              initialActivePath={state.checkpoint.active_path}
              initialView={state.checkpoint.view}
              initialSelections={state.checkpoint.selections}
              initialRevision={state.source.source_revision}
              languageServiceForPath={languageServiceForPath}
              onPublishDraft={publishDraft}
              onDraftStateChange={setHasLocalDraft}
              onCheckpointStateChange={setPendingCheckpoint}
              checkpointStatus={checkpointStatus}
              checkpointError={checkpointError}
              publishStatus={publishStatus}
              publishError={publishError}
              historyEntries={historyState.kind === "ready" &&
                  historyState.activeArtifactId === state.source.artifact_id
                ? historyState.entries
                : []}
              historyStatus={historyState.kind === "ready" &&
                  historyState.activeArtifactId !== state.source.artifact_id
                ? "loading"
                : historyState.kind}
              historyError={historyState.kind === "failed"
                ? historyState.message
                : undefined}
              onLoadHistorySource={loadHistorySource}
              runtimeTraceEvents={runtimeTraceEvents}
              runtimeTraceProjectionDigest={runtimeTraceProjectionDigest}
              runtimeTraceStatus={runtimeTraceStatus}
              runtimeTraceError={runtimeTraceError}
              onRuntimeTrace={recordRuntimeTrace}
              onPrepareBugCapsule={prepareBugCapsule}
            />
            {context && (
              <VisualQualityGate
                key={`quality:${state.source.artifact_id}:${state.source.source_revision}`}
                artifactId={state.source.artifact_id}
                sourceRevision={state.source.source_revision}
                sessionId={context.sessionId}
                token={context.token}
              />
            )}
          </div>
          {workspacePanel === "mapping" && (
            <WorkspaceApplyComposer
              sourcePaths={Object.keys(state.source.files)}
              busy={previewStarting}
              error={applyError}
              onCancel={() => setWorkspacePanel("none")}
              onReview={reviewWorkspace}
            />
          )}
          {workspacePanel === "review" && applyPreview && (
            <WorkspaceReview
              key={applyPreview.review_digest}
              preview={applyPreview}
              review={applyReview}
              busy={applyStarting}
              error={applyError}
              onBack={() => setWorkspacePanel("none")}
              onApply={(selections) => applyWorkspace(applyPreview, selections)}
            />
          )}
        </div>
      )}
      <footer className="artifact-surface-footer">
        <span className="artifact-source-meta">
          Rust source r{state.kind === "ready"
            ? state.source.source_revision
            : "—"}
          {state.kind === "ready" &&
            ` · ${
              Object.keys(state.source.files).length
            } bounded virtual file(s)`}
        </span>
        {state.kind === "ready" && workspacePanel !== "mapping" && (
          <div className="workspace-apply-form">
            {applyPreview && workspacePanel !== "review" && (
              <button
                type="button"
                onClick={() => setWorkspacePanel("review")}
              >
                Show {applyPreview.changes.length} review(s)
              </button>
            )}
            <button
              type="button"
              data-state={applyStatus}
              disabled={hasLocalDraft || applyBusy}
              title={hasLocalDraft
                ? "Publish the local Artifact draft before applying it to the workspace"
                : "Map Artifact files to explicit workspace targets before creating one WorkspaceWrite Approval Block"}
              onClick={() => {
                setApplyError(undefined);
                setApplyPreview(undefined);
                setApplyReview(undefined);
                setWorkspacePanel("mapping");
              }}
            >
              {workspaceApplyLabel(
                previewStarting || applyStarting ? "applying" : applyStatus,
                Object.keys(state.source.files).length,
              )}
            </button>
          </div>
        )}
        {applyError && workspacePanel === "none" && (
          <strong className="workspace-apply-error" role="alert">
            {applyError}
          </strong>
        )}
      </footer>
    </main>
  );
}

function workspaceApplyLabel(
  status: WorkspaceApplyStatus | "idle",
  fileCount: number,
): string {
  switch (status) {
    case "waiting_approval":
      return "Approve in Agent";
    case "applying":
      return "Applying";
    case "applied":
      return "Applied";
    case "rejected":
      return "Review again";
    case "failed":
      return "Retry review";
    case "unknown_execution":
      return "Inspect target";
    case "idle":
      return `Map ${Math.min(fileCount, 32)} file(s)`;
  }
}

async function appendRuntimeTraceWithRetry(
  client: RuntimeTraceClient,
  events: RuntimeTraceInput[],
): Promise<RuntimeTraceProjection> {
  try {
    return await client.append(events);
  } catch {
    // The Rust store makes an exact retry idempotent, including the case where
    // the first response was lost after the append reached durable storage.
    return await client.append(events);
  }
}

async function fetchArtifactSource(
  context: ArtifactContext,
  artifactId: string,
  signal: AbortSignal,
): Promise<ArtifactSourceResponse> {
  const query = new URLSearchParams({
    token: context.token,
    session_id: String(context.sessionId),
  });
  const response = await fetch(
    `/agent/artifact/${encodeURIComponent(artifactId)}/source?${query}`,
    { cache: "no-store", signal },
  );
  if (!response.ok) {
    throw new Error(`Rust source endpoint returned ${response.status}.`);
  }
  const source = await response.json() as ArtifactSourceResponse;
  const files = source && typeof source.files === "object" &&
      source.files !== null && !Array.isArray(source.files)
    ? Object.entries(source.files)
    : [];
  const totalSourceBytes = files.reduce(
    (total, [, value]) =>
      total +
      (typeof value === "string"
        ? new TextEncoder().encode(value).byteLength
        : 1024 * 1024 + 1),
    0,
  );
  if (
    source.artifact_id !== artifactId ||
    !Number.isSafeInteger(source.source_revision) ||
    source.source_revision < 1 ||
    typeof source.entrypoint !== "string" ||
    files.length === 0 ||
    files.length > 100 ||
    files.some(([path, value]) =>
      !path.startsWith("/") || path.includes("..") || path.includes("\\") ||
      typeof value !== "string"
    ) ||
    totalSourceBytes > 1024 * 1024 ||
    typeof source.files[source.entrypoint] !== "string"
  ) {
    throw new Error("Rust source snapshot did not match the active artifact.");
  }
  return source;
}

async function fetchArtifactWorkspace(
  context: ArtifactContext,
  artifactId: string,
  signal: AbortSignal,
): Promise<{
  source: ArtifactSourceResponse;
  checkpoint: ArtifactEditorCheckpoint;
}> {
  const source = await fetchArtifactSource(context, artifactId, signal);
  const checkpoint = await new ArtifactEditorCheckpointClient({
    artifactId: source.artifact_id,
    sourceRevision: source.source_revision,
    entrypoint: source.entrypoint,
    files: source.files,
    sessionId: context.sessionId,
    token: context.token,
  }).load(signal);
  return { source, checkpoint };
}

function checkpointFingerprint(input: ArtifactEditorCheckpointInput): string {
  return JSON.stringify({
    files: Object.fromEntries(
      Object.keys(input.files).sort().map((path) => [path, input.files[path]]),
    ),
    activePath: input.activePath,
    view: input.view,
    selections: Object.fromEntries(
      Object.keys(input.selections).sort().map((path) => [
        path,
        input.selections[path],
      ]),
    ),
  });
}

function cloneCheckpointInput(
  input: ArtifactEditorCheckpointInput,
): ArtifactEditorCheckpointInput {
  return {
    files: { ...input.files },
    activePath: input.activePath,
    view: input.view,
    selections: Object.fromEntries(
      Object.entries(input.selections).map(([path, selection]) => [
        path,
        { ...selection },
      ]),
    ),
  };
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

function readArtifactContext(): ArtifactContext | undefined {
  const query = new URLSearchParams(globalThis.location.search);
  const artifactId = query.get("artifact_id") ?? "";
  const token = query.get("token") ?? "";
  const sessionId = Number(query.get("session_id"));
  if (
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
      .test(artifactId) ||
    !/^[A-Za-z0-9_-]{32,128}$/.test(token) ||
    !Number.isSafeInteger(sessionId) ||
    sessionId < 1 ||
    sessionId > 999
  ) return undefined;
  return { artifactId, sessionId, token };
}
