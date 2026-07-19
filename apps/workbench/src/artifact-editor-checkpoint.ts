export type ArtifactEditorView = "code" | "diff" | "trace";

export interface ArtifactEditorSelection {
  anchor: number;
  head: number;
}

export interface ArtifactEditorCheckpoint {
  schema_version: number;
  artifact_id: string;
  base_source_revision: number;
  revision: number;
  state_digest: string;
  entrypoint: string;
  files: Record<string, string>;
  active_path: string;
  view: ArtifactEditorView;
  selections: Record<string, ArtifactEditorSelection>;
}

export interface ArtifactEditorCheckpointInput {
  files: Record<string, string>;
  activePath: string;
  view: ArtifactEditorView;
  selections: Record<string, ArtifactEditorSelection>;
}

export interface ArtifactEditorCheckpointContext {
  artifactId: string;
  sourceRevision: number;
  entrypoint: string;
  files: Record<string, string>;
  sessionId: number;
  token: string;
}

type Fetcher = typeof fetch;

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const MAX_SOURCE_BYTES = 1024 * 1024;

export class ArtifactEditorCheckpointClient {
  constructor(
    private readonly context: ArtifactEditorCheckpointContext,
    private readonly fetcher: Fetcher = (input, init) =>
      globalThis.fetch(input, init),
  ) {
    validateFiles(context.files, context.files);
  }

  async load(signal: AbortSignal): Promise<ArtifactEditorCheckpoint> {
    return await this.#request("GET", undefined, signal);
  }

  async save(
    expectedRevision: number,
    input: ArtifactEditorCheckpointInput,
    signal: AbortSignal,
  ): Promise<ArtifactEditorCheckpoint> {
    if (!Number.isSafeInteger(expectedRevision) || expectedRevision < 0) {
      throw new Error("Artifact editor checkpoint revision is invalid.");
    }
    validateInput(this.context.files, input);
    return await this.#request("PUT", {
      expected_revision: expectedRevision,
      base_source_revision: this.context.sourceRevision,
      files: input.files,
      active_path: input.activePath,
      view: input.view,
      selections: input.selections,
    }, signal);
  }

  async #request(
    method: "GET" | "PUT",
    body: Record<string, unknown> | undefined,
    signal: AbortSignal,
  ): Promise<ArtifactEditorCheckpoint> {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
    });
    const response = await this.fetcher(
      `/agent/artifact/${
        encodeURIComponent(this.context.artifactId)
      }/editor-state?${query}`,
      {
        method,
        cache: "no-store",
        ...(body
          ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          }
          : {}),
        signal,
      },
    );
    if (!response.ok) {
      throw new Error(
        response.status === 409
          ? "Artifact editor checkpoint is stale; reload the current Rust state."
          : `Rust artifact editor endpoint returned ${response.status}.`,
      );
    }
    const checkpoint = await response.json() as ArtifactEditorCheckpoint;
    if (!validCheckpoint(checkpoint, this.context)) {
      throw new Error(
        "Rust artifact editor checkpoint did not match the current Artifact.",
      );
    }
    return checkpoint;
  }
}

function validateInput(
  baseline: Record<string, string>,
  input: ArtifactEditorCheckpointInput,
): void {
  validateFiles(baseline, input.files);
  if (
    !(input.activePath in input.files) ||
    !["code", "diff", "trace"].includes(input.view) ||
    !validSelections(input.selections, input.files)
  ) {
    throw new Error("Artifact editor checkpoint state is invalid.");
  }
}

function validateFiles(
  baseline: Record<string, string>,
  files: Record<string, string>,
): void {
  const baselinePaths = Object.keys(baseline).sort();
  const paths = Object.keys(files).sort();
  const bytes = paths.reduce(
    (total, path) =>
      total +
      (typeof files[path] === "string"
        ? new TextEncoder().encode(files[path]).byteLength
        : MAX_SOURCE_BYTES + 1),
    0,
  );
  if (
    paths.length === 0 || paths.length > 100 ||
    paths.length !== baselinePaths.length ||
    paths.some((path, index) =>
      path !== baselinePaths[index] || !path.startsWith("/") ||
      path.includes("..") || path.includes("\\")
    ) ||
    bytes > MAX_SOURCE_BYTES
  ) {
    throw new Error("Artifact editor checkpoint changed its fixed file set.");
  }
}

function validCheckpoint(
  checkpoint: ArtifactEditorCheckpoint,
  context: ArtifactEditorCheckpointContext,
): boolean {
  try {
    if (
      !checkpoint || checkpoint.schema_version !== 1 ||
      checkpoint.artifact_id !== context.artifactId ||
      checkpoint.base_source_revision !== context.sourceRevision ||
      !Number.isSafeInteger(checkpoint.revision) || checkpoint.revision < 0 ||
      !SHA256_PATTERN.test(checkpoint.state_digest) ||
      checkpoint.entrypoint !== context.entrypoint ||
      !(checkpoint.active_path in checkpoint.files) ||
      !["code", "diff", "trace"].includes(checkpoint.view) ||
      !validSelections(checkpoint.selections, checkpoint.files)
    ) return false;
    validateFiles(context.files, checkpoint.files);
    return true;
  } catch {
    return false;
  }
}

function validSelections(
  selections: Record<string, ArtifactEditorSelection>,
  files: Record<string, string>,
): boolean {
  if (
    !selections || typeof selections !== "object" ||
    Array.isArray(selections) || Object.keys(selections).length > 100
  ) return false;
  return Object.entries(selections).every(([path, selection]) =>
    path in files && selection &&
    Number.isSafeInteger(selection.anchor) && selection.anchor >= 0 &&
    selection.anchor <= files[path].length &&
    Number.isSafeInteger(selection.head) && selection.head >= 0 &&
    selection.head <= files[path].length
  );
}
