export type ArtifactDraftStatus =
  | "waiting_approval"
  | "compiling"
  | "accepted"
  | "rejected"
  | "failed";

export interface PublishedArtifact {
  artifact_id: string;
  source_revision: number;
  entrypoint: string;
  content_digest: string;
}

export interface ArtifactDraftUpdate {
  operation_id: string;
  operation_revision: number;
  status: ArtifactDraftStatus;
  artifact?: PublishedArtifact;
  error?: string;
}

export interface ArtifactDraftPublisherContext {
  artifactId: string;
  sourceRevision: number;
  entrypoint: string;
  files: Record<string, string>;
  sessionId: number;
  token: string;
}

type Fetcher = typeof fetch;
type Sleeper = (delay: number, signal: AbortSignal) => Promise<void>;

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export class ArtifactDraftPublisher {
  constructor(
    private readonly context: ArtifactDraftPublisherContext,
    private readonly fetcher: Fetcher = (input, init) =>
      globalThis.fetch(input, init),
    private readonly sleeper: Sleeper = abortableDelay,
  ) {}

  async publish(
    files: Record<string, string>,
    onUpdate: (update: ArtifactDraftUpdate) => void,
    signal: AbortSignal,
  ): Promise<PublishedArtifact> {
    validateDraftFiles(this.context.files, files);
    let update = await this.#request("POST", undefined, files, signal);
    onUpdate(update);
    while (
      update.status === "waiting_approval" || update.status === "compiling"
    ) {
      await this.sleeper(350, signal);
      update = await this.#request(
        "GET",
        update.operation_id,
        undefined,
        signal,
      );
      onUpdate(update);
    }
    if (update.status === "accepted" && update.artifact) {
      return update.artifact;
    }
    if (update.status === "rejected") {
      throw new Error("Artifact publish was rejected.");
    }
    throw new Error(update.error ?? "Artifact publish failed.");
  }

  async #request(
    method: "GET" | "POST",
    operationId: string | undefined,
    files: Record<string, string> | undefined,
    signal: AbortSignal,
  ): Promise<ArtifactDraftUpdate> {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
      ...(operationId ? { operation_id: operationId } : {}),
    });
    const response = await this.fetcher(
      `/agent/artifact/${
        encodeURIComponent(this.context.artifactId)
      }/draft?${query}`,
      {
        method,
        cache: "no-store",
        ...(files
          ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              base_source_revision: this.context.sourceRevision,
              entrypoint: this.context.entrypoint,
              files,
            }),
          }
          : {}),
        signal,
      },
    );
    if (!response.ok) {
      throw new Error(
        `Rust artifact draft endpoint returned ${response.status}.`,
      );
    }
    const payload = await response.json() as ArtifactDraftUpdate;
    if (
      !validUpdate(
        payload,
        this.context.sourceRevision,
        this.context.entrypoint,
      )
    ) {
      throw new Error(
        "Rust artifact draft response did not match the editor context.",
      );
    }
    return payload;
  }
}

function validateDraftFiles(
  baseline: Record<string, string>,
  draft: Record<string, string>,
): void {
  const baselinePaths = Object.keys(baseline).sort();
  const draftPaths = Object.keys(draft).sort();
  if (
    baselinePaths.length !== draftPaths.length ||
    baselinePaths.some((path, index) => path !== draftPaths[index])
  ) {
    throw new Error("Artifact draft file set changed outside Rust authority.");
  }
  let bytes = 0;
  for (const path of draftPaths) {
    const source = draft[path];
    if (typeof source !== "string") {
      throw new Error("Artifact draft contains an invalid source file.");
    }
    bytes += new TextEncoder().encode(source).byteLength;
  }
  if (bytes > 1024 * 1024) {
    throw new Error("Artifact draft exceeds the 1 MiB source bound.");
  }
}

function validUpdate(
  update: ArtifactDraftUpdate,
  baseRevision: number,
  entrypoint: string,
): boolean {
  if (
    !update || !UUID_PATTERN.test(update.operation_id) ||
    !Number.isSafeInteger(update.operation_revision) ||
    update.operation_revision < 1 ||
    ![
      "waiting_approval",
      "compiling",
      "accepted",
      "rejected",
      "failed",
    ].includes(update.status)
  ) return false;
  if (update.status !== "accepted") return update.artifact === undefined;
  return Boolean(
    update.artifact && UUID_PATTERN.test(update.artifact.artifact_id) &&
      update.artifact.source_revision === baseRevision + 1 &&
      update.artifact.entrypoint === entrypoint &&
      /^[0-9a-f]{64}$/.test(update.artifact.content_digest),
  );
}

function abortableDelay(delay: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason);
      return;
    }
    const timer = globalThis.setTimeout(resolve, delay);
    signal.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(signal.reason);
    }, { once: true });
  });
}
