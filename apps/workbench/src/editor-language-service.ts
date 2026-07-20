export interface EditorPosition {
  line: number;
  character: number;
}

export interface EditorDiagnostic {
  severity: "error" | "warning" | "information" | "hint";
  message: string;
  start: EditorPosition;
  end: EditorPosition;
}

export interface EditorCompletion {
  label: string;
  insert_text: string;
  detail?: string;
  kind?: number;
}

export interface EditorLanguageService {
  diagnostics(
    draftFiles: Readonly<Record<string, string>>,
  ): Promise<EditorDiagnostic[]>;
  completions(
    draftFiles: Readonly<Record<string, string>>,
    position: EditorPosition,
    signal: AbortSignal,
  ): Promise<EditorCompletion[]>;
}

interface EditorLspResponse {
  artifact_id: string;
  source_revision: number;
  document_path: string;
  document_version: number;
  kind: "diagnostics" | "completion";
  diagnostics: EditorDiagnostic[];
  completions: EditorCompletion[];
}

export interface ArtifactLanguageServiceContext {
  artifactId: string;
  sourceRevision: number;
  documentPath: string;
  sessionId: number;
  token: string;
}

type Fetcher = typeof fetch;

export class ArtifactLanguageService implements EditorLanguageService {
  #diagnosticController?: AbortController;

  constructor(
    private readonly context: ArtifactLanguageServiceContext,
    private readonly fetcher: Fetcher = (input, init) =>
      globalThis.fetch(input, init),
  ) {}

  diagnostics(
    draftFiles: Readonly<Record<string, string>>,
  ): Promise<EditorDiagnostic[]> {
    this.#diagnosticController?.abort();
    const controller = new AbortController();
    this.#diagnosticController = controller;
    return this.#request({ kind: "diagnostics", draftFiles }, controller.signal)
      .then((response) => response.diagnostics)
      .finally(() => {
        if (this.#diagnosticController === controller) {
          this.#diagnosticController = undefined;
        }
      });
  }

  completions(
    draftFiles: Readonly<Record<string, string>>,
    position: EditorPosition,
    signal: AbortSignal,
  ): Promise<EditorCompletion[]> {
    return this.#request({ kind: "completion", draftFiles, position }, signal)
      .then((response) => response.completions);
  }

  async #request(
    request:
      | {
        kind: "diagnostics";
        draftFiles: Readonly<Record<string, string>>;
      }
      | {
        kind: "completion";
        draftFiles: Readonly<Record<string, string>>;
        position: EditorPosition;
      },
    signal: AbortSignal,
  ): Promise<EditorLspResponse> {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
    });
    const response = await this.fetcher(
      `/agent/artifact/${
        encodeURIComponent(this.context.artifactId)
      }/lsp?${query}`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          source_revision: this.context.sourceRevision,
          document_path: this.context.documentPath,
          draft_files: request.draftFiles,
          kind: request.kind,
          ...(request.kind === "completion"
            ? { position: request.position }
            : {}),
        }),
        signal,
      },
    );
    if (!response.ok) {
      throw new Error(`Rust Deno LSP endpoint returned ${response.status}.`);
    }
    const payload = await response.json() as EditorLspResponse;
    if (
      payload.artifact_id !== this.context.artifactId ||
      payload.source_revision !== this.context.sourceRevision ||
      payload.document_path !== this.context.documentPath ||
      payload.kind !== request.kind ||
      !Number.isSafeInteger(payload.document_version) ||
      payload.document_version < 1 ||
      !Array.isArray(payload.diagnostics) ||
      !Array.isArray(payload.completions)
    ) {
      throw new Error(
        "Rust Deno LSP response did not match the editor context.",
      );
    }
    return payload;
  }
}
