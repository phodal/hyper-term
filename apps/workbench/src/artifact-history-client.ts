export interface ArtifactHistoryContext {
  activeArtifactId: string;
  sessionId: number;
  token: string;
}

export interface ArtifactHistoryEntry {
  event_sequence: number;
  recorded_at_ms: number;
  operation_id: string | null;
  artifact: {
    artifact_id: string;
    source_revision: number;
    entrypoint: string;
    content_digest: string;
    compiler: { name: "esbuild-wasm"; version: string };
  };
}

export interface ArtifactHistorySource {
  artifact_id: string;
  source_revision: number;
  entrypoint: string;
  files: Record<string, string>;
}

type Fetch = typeof globalThis.fetch;

const uuidPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const sha256Pattern = /^[0-9a-f]{64}$/;

export class ArtifactHistoryClient {
  constructor(
    private readonly context: ArtifactHistoryContext,
    private readonly fetcher: Fetch = (input, init) =>
      globalThis.fetch(input, init),
  ) {}

  async list(signal?: AbortSignal): Promise<ArtifactHistoryEntry[]> {
    const response = await this.fetcher(
      this.url(
        `/agent/artifact/${
          encodeURIComponent(this.context.activeArtifactId)
        }/history`,
      ),
      { cache: "no-store", signal },
    );
    if (!response.ok) {
      throw new Error(`Rust history endpoint returned ${response.status}.`);
    }
    const payload = await response.json() as {
      active_artifact_id?: unknown;
      entries?: unknown;
    };
    if (
      payload.active_artifact_id !== this.context.activeArtifactId ||
      !Array.isArray(payload.entries) ||
      payload.entries.length === 0 ||
      payload.entries.length > 64 ||
      !payload.entries.every(validHistoryEntry)
    ) {
      throw new Error(
        "Rust history response violated the Artifact timeline contract.",
      );
    }
    const entries = payload.entries as ArtifactHistoryEntry[];
    if (
      entries[0].artifact.artifact_id !== this.context.activeArtifactId ||
      entries.some((entry, index) =>
        index > 0 &&
        entry.event_sequence >= entries[index - 1].event_sequence
      ) ||
      new Set(entries.map((entry) => entry.artifact.artifact_id)).size !==
        entries.length
    ) {
      throw new Error(
        "Rust history response is not a newest-first unique timeline.",
      );
    }
    return entries;
  }

  async source(
    entry: ArtifactHistoryEntry,
    signal?: AbortSignal,
  ): Promise<ArtifactHistorySource> {
    const response = await this.fetcher(
      this.url(
        `/agent/artifact/${
          encodeURIComponent(this.context.activeArtifactId)
        }/history/${encodeURIComponent(entry.artifact.artifact_id)}/source`,
      ),
      { cache: "no-store", signal },
    );
    if (!response.ok) {
      throw new Error(
        `Rust historical source endpoint returned ${response.status}.`,
      );
    }
    const source = await response.json() as ArtifactHistorySource;
    if (!validHistorySource(source, entry)) {
      throw new Error(
        "Rust historical source did not match its journal revision.",
      );
    }
    return source;
  }

  private url(path: string): string {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
    });
    return `${path}?${query}`;
  }
}

function validHistoryEntry(value: unknown): value is ArtifactHistoryEntry {
  if (!value || typeof value !== "object") return false;
  const entry = value as Partial<ArtifactHistoryEntry>;
  const artifact = entry.artifact;
  return Number.isSafeInteger(entry.event_sequence) &&
    Number(entry.event_sequence) >= 1 &&
    Number.isSafeInteger(entry.recorded_at_ms) &&
    Number(entry.recorded_at_ms) >= 1 &&
    (entry.operation_id === null ||
      typeof entry.operation_id === "string" &&
        uuidPattern.test(entry.operation_id)) &&
    Boolean(artifact) &&
    typeof artifact === "object" &&
    typeof artifact.artifact_id === "string" &&
    uuidPattern.test(artifact.artifact_id) &&
    Number.isSafeInteger(artifact.source_revision) &&
    Number(artifact.source_revision) >= 1 &&
    typeof artifact.entrypoint === "string" &&
    validVirtualPath(artifact.entrypoint) &&
    typeof artifact.content_digest === "string" &&
    sha256Pattern.test(artifact.content_digest) &&
    artifact.compiler?.name === "esbuild-wasm" &&
    typeof artifact.compiler.version === "string" &&
    artifact.compiler.version.length >= 1 &&
    artifact.compiler.version.length <= 64;
}

function validHistorySource(
  source: ArtifactHistorySource,
  entry: ArtifactHistoryEntry,
): boolean {
  if (
    !source || typeof source !== "object" ||
    source.artifact_id !== entry.artifact.artifact_id ||
    source.source_revision !== entry.artifact.source_revision ||
    source.entrypoint !== entry.artifact.entrypoint ||
    !source.files || typeof source.files !== "object" ||
    Array.isArray(source.files)
  ) return false;
  const files = Object.entries(source.files);
  if (
    files.length === 0 || files.length > 100 ||
    files.some(([path, value]) =>
      !validVirtualPath(path) || typeof value !== "string"
    ) ||
    typeof source.files[source.entrypoint] !== "string"
  ) return false;
  const bytes = files.reduce(
    (total, [, value]) => total + new TextEncoder().encode(value).byteLength,
    0,
  );
  return bytes <= 1024 * 1024;
}

function validVirtualPath(path: string): boolean {
  return path.startsWith("/") && path.length <= 4096 &&
    !path.includes("..") && !path.includes("\\");
}
