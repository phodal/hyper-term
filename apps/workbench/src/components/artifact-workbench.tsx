import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArtifactDraftPublisher,
  type ArtifactDraftStatus,
} from "../artifact-draft-publisher.ts";
import { resolveHost } from "../host.ts";
import { ArtifactLanguageService } from "../editor-language-service.ts";
import { GenUiStudio } from "./genui-studio.tsx";

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
  const publishController = useRef<AbortController | undefined>(undefined);
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

  useEffect(() => () => publishController.current?.abort(), []);

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
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setPublishStatus("failed");
      setPublishError(error instanceof Error ? error.message : String(error));
    });
  }, [context, state]);

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
        <span>local draft · no workspace write</span>
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
        <GenUiStudio
          key={`${state.source.artifact_id}:${state.source.source_revision}`}
          host={host}
          initialSource={state.source.files[state.source.entrypoint]}
          baselineSource={state.source.files[state.source.entrypoint]}
          initialRevision={state.source.source_revision}
          heading={state.source.entrypoint}
          languageService={languageService}
          onPublishDraft={publishDraft}
          publishStatus={publishStatus}
          publishError={publishError}
        />
      )}
      <footer className="artifact-surface-footer">
        Rust source r{state.kind === "ready"
          ? state.source.source_revision
          : "—"}
        {state.kind === "ready" &&
          ` · ${
            Object.keys(state.source.files).length
          } bounded virtual file(s)`}
        <span>
          Apply remains behind a future permission-broker transaction.
        </span>
      </footer>
    </main>
  );
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
