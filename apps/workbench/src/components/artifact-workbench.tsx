import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArtifactDraftPublisher,
  type ArtifactDraftStatus,
} from "../artifact-draft-publisher.ts";
import { resolveHost } from "../host.ts";
import { ArtifactLanguageService } from "../editor-language-service.ts";
import {
  WorkspaceApplyPublisher,
  type WorkspaceApplyStatus,
  type WorkspaceApplyUpdate,
} from "../workspace-apply-publisher.ts";
import { GenUiStudio } from "./genui-studio.tsx";
import { WorkspaceReview } from "./workspace-review.tsx";

interface ArtifactSourceResponse {
  artifact_id: string;
  source_revision: number;
  entrypoint: string;
  files: Record<string, string>;
}

type LoadState =
  | { kind: "loading" }
  | { kind: "failed"; message: string }
  | { kind: "ready"; source: ArtifactSourceResponse };

interface ArtifactContext {
  artifactId: string;
  sessionId: number;
  token: string;
}

export function ArtifactWorkbench() {
  const host = useMemo(resolveHost, []);
  const context = useMemo(readArtifactContext, []);
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [publishStatus, setPublishStatus] = useState<
    ArtifactDraftStatus | "idle"
  >("idle");
  const [publishError, setPublishError] = useState<string>();
  const [workspaceTarget, setWorkspaceTarget] = useState("");
  const [hasLocalDraft, setHasLocalDraft] = useState(false);
  const [applyReview, setApplyReview] = useState<WorkspaceApplyUpdate>();
  const [applyError, setApplyError] = useState<string>();
  const [reviewVisible, setReviewVisible] = useState(false);
  const publishController = useRef<AbortController | undefined>(undefined);
  const applyController = useRef<AbortController | undefined>(undefined);
  const openedReviewOperation = useRef<string | undefined>(undefined);
  const languageService = useMemo(() => {
    if (!context || state.kind !== "ready") return undefined;
    return new ArtifactLanguageService({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      documentPath: state.source.entrypoint,
      sessionId: context.sessionId,
      token: context.token,
    });
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
    fetchArtifactSource(context, context.artifactId, controller.signal)
      .then((source) => {
        setState({ kind: "ready", source });
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

  useEffect(() => () => {
    publishController.current?.abort();
    applyController.current?.abort();
  }, []);

  const publishDraft = useCallback((source: string) => {
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
      source,
      (update) => setPublishStatus(update.status),
      controller.signal,
    ).then((artifact) =>
      fetchArtifactSource(context, artifact.artifact_id, controller.signal)
    ).then((nextSource) => {
      if (controller.signal.aborted) return;
      setState({ kind: "ready", source: nextSource });
      setPublishStatus("accepted");
      setHasLocalDraft(false);
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setPublishStatus("failed");
      setPublishError(error instanceof Error ? error.message : String(error));
    });
  }, [context, state]);

  const applyWorkspace = useCallback(() => {
    if (!context || state.kind !== "ready") return;
    const targetPath = workspaceTarget.trim();
    if (!targetPath || hasLocalDraft) return;
    applyController.current?.abort();
    const controller = new AbortController();
    applyController.current = controller;
    setApplyError(undefined);
    setApplyReview(undefined);
    setReviewVisible(false);
    const publisher = new WorkspaceApplyPublisher({
      artifactId: state.source.artifact_id,
      sourceRevision: state.source.source_revision,
      sourcePath: state.source.entrypoint,
      sessionId: context.sessionId,
      token: context.token,
    });
    publisher.apply(
      targetPath,
      (update) => {
        setApplyReview(update);
        if (openedReviewOperation.current !== update.operation_id) {
          openedReviewOperation.current = update.operation_id;
          setReviewVisible(true);
        }
      },
      controller.signal,
    ).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setApplyError(error instanceof Error ? error.message : String(error));
    });
  }, [context, hasLocalDraft, state, workspaceTarget]);

  const applyStatus: WorkspaceApplyStatus | "idle" = applyReview?.status ??
    "idle";
  const applyBusy = applyStatus === "waiting_approval" ||
    applyStatus === "applying";

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
        reviewVisible && applyReview
          ? (
            <WorkspaceReview
              review={applyReview}
              status={applyReview.status}
              error={applyError}
              onBack={() => setReviewVisible(false)}
            />
          )
          : (
            <GenUiStudio
              key={`${state.source.artifact_id}:${state.source.source_revision}`}
              host={host}
              initialSource={state.source.files[state.source.entrypoint]}
              baselineSource={state.source.files[state.source.entrypoint]}
              initialRevision={state.source.source_revision}
              heading={state.source.entrypoint}
              languageService={languageService}
              onPublishDraft={publishDraft}
              onDraftStateChange={setHasLocalDraft}
              publishStatus={publishStatus}
              publishError={publishError}
            />
          )
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
        {state.kind === "ready" && (
          <form
            className="workspace-apply-form"
            onSubmit={(event) => {
              event.preventDefault();
              applyWorkspace();
            }}
          >
            <input
              aria-label="Workspace target path"
              autoComplete="off"
              disabled={applyBusy}
              maxLength={4096}
              placeholder="workspace path, e.g. src/App.tsx"
              spellCheck={false}
              value={workspaceTarget}
              onChange={(event) => setWorkspaceTarget(event.target.value)}
            />
            {applyReview && !reviewVisible && (
              <button
                type="button"
                onClick={() => setReviewVisible(true)}
              >
                Show diff
              </button>
            )}
            <button
              type="submit"
              data-state={applyStatus}
              disabled={!workspaceTarget.trim() || hasLocalDraft || applyBusy}
              title={hasLocalDraft
                ? "Publish the local Artifact draft before applying it to the workspace"
                : "Ask Rust to capture the exact workspace base and create a WorkspaceWrite Approval Block"}
            >
              {workspaceApplyLabel(applyStatus)}
            </button>
          </form>
        )}
        {applyError && !reviewVisible && (
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
      return "Review apply";
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
  if (
    source.artifact_id !== artifactId ||
    !Number.isSafeInteger(source.source_revision) ||
    source.source_revision < 1 ||
    typeof source.entrypoint !== "string" ||
    typeof source.files[source.entrypoint] !== "string"
  ) {
    throw new Error("Rust source snapshot did not match the active artifact.");
  }
  return source;
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
