import { useEffect, useMemo, useState } from "react";
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
  const languageService = useMemo(() => {
    if (!context || state.kind !== "ready") return undefined;
    return new ArtifactLanguageService({
      artifactId: context.artifactId,
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
    const query = new URLSearchParams({
      token: context.token,
      session_id: String(context.sessionId),
    });
    fetch(
      `/agent/artifact/${
        encodeURIComponent(context.artifactId)
      }/source?${query}`,
      { cache: "no-store", signal: controller.signal },
    )
      .then(async (response) => {
        if (!response.ok) {
          throw new Error(`Rust source endpoint returned ${response.status}.`);
        }
        return await response.json() as ArtifactSourceResponse;
      })
      .then((source) => {
        if (
          source.artifact_id !== context.artifactId ||
          !Number.isSafeInteger(source.source_revision) ||
          source.source_revision < 1 ||
          typeof source.entrypoint !== "string" ||
          typeof source.files[source.entrypoint] !== "string"
        ) {
          throw new Error(
            "Rust source snapshot did not match the active artifact.",
          );
        }
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
