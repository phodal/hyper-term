export type WorkspaceApplyStatus =
  | "waiting_approval"
  | "applying"
  | "applied"
  | "rejected"
  | "failed"
  | "unknown_execution";

export interface WorkspaceApplyMapping {
  source_path: string;
  target_path: string;
}

export interface WorkspaceApplyChange extends WorkspaceApplyMapping {
  base_digest: string | null;
  proposed_digest: string;
  before: string;
  after: string;
}

export interface WorkspaceApplyUpdate {
  operation_id: string;
  operation_revision: number;
  status: WorkspaceApplyStatus;
  artifact_source_revision: number;
  source_path: string;
  target_path: string;
  base_digest: string | null;
  proposed_digest: string;
  before: string;
  after: string;
  transaction_digest: string;
  changes: WorkspaceApplyChange[];
  error?: string;
}

export interface WorkspaceApplyPublisherContext {
  artifactId: string;
  sourceRevision: number;
  sessionId: number;
  token: string;
}

type Fetcher = typeof fetch;
type Sleeper = (delay: number, signal: AbortSignal) => Promise<void>;

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export class WorkspaceApplyPublisher {
  constructor(
    private readonly context: WorkspaceApplyPublisherContext,
    private readonly fetcher: Fetcher = (input, init) =>
      globalThis.fetch(input, init),
    private readonly sleeper: Sleeper = abortableDelay,
  ) {}

  async apply(
    mappings: WorkspaceApplyMapping[],
    onUpdate: (update: WorkspaceApplyUpdate) => void,
    signal: AbortSignal,
  ): Promise<WorkspaceApplyUpdate> {
    if (!validMappings(mappings)) {
      throw new Error("Workspace apply mappings are invalid or ambiguous.");
    }
    const exactMappings = mappings.map((mapping) => ({ ...mapping }));
    let update = await this.#request(
      "POST",
      exactMappings,
      undefined,
      signal,
    );
    onUpdate(update);
    while (
      update.status === "waiting_approval" || update.status === "applying"
    ) {
      await this.sleeper(350, signal);
      update = await this.#request(
        "GET",
        exactMappings,
        update.operation_id,
        signal,
      );
      onUpdate(update);
    }
    if (update.status === "applied") return update;
    if (update.status === "rejected") {
      throw new Error("Workspace apply was rejected.");
    }
    if (update.status === "unknown_execution") {
      throw new Error(
        update.error ??
          "Workspace apply has an unknown execution outcome. Inspect the target before retrying.",
      );
    }
    throw new Error(update.error ?? "Workspace apply failed.");
  }

  async #request(
    method: "GET" | "POST",
    mappings: WorkspaceApplyMapping[],
    operationId: string | undefined,
    signal: AbortSignal,
  ): Promise<WorkspaceApplyUpdate> {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
      ...(operationId ? { operation_id: operationId } : {}),
    });
    const response = await this.fetcher(
      `/agent/artifact/${
        encodeURIComponent(this.context.artifactId)
      }/workspace-apply?${query}`,
      {
        method,
        cache: "no-store",
        ...(method === "POST"
          ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              artifact_source_revision: this.context.sourceRevision,
              mappings,
            }),
          }
          : {}),
        signal,
      },
    );
    if (!response.ok) {
      const detail = (await response.text()).trim();
      throw new Error(
        detail || `Rust workspace apply endpoint returned ${response.status}.`,
      );
    }
    const payload = await response.json() as WorkspaceApplyUpdate;
    if (!validUpdate(payload, this.context, mappings)) {
      throw new Error(
        "Rust workspace apply response did not match the editor context.",
      );
    }
    return payload;
  }
}

function validUpdate(
  update: WorkspaceApplyUpdate,
  context: WorkspaceApplyPublisherContext,
  mappings: WorkspaceApplyMapping[],
): boolean {
  const expected = new Map(
    mappings.map((mapping) => [mapping.source_path, mapping.target_path]),
  );
  const changes = Array.isArray(update?.changes) ? update.changes : [];
  const totalBytes = changes.reduce(
    (total, change) =>
      total + new TextEncoder().encode(change.before).byteLength +
      new TextEncoder().encode(change.after).byteLength,
    0,
  );
  const first = changes[0];
  return Boolean(
    update && UUID_PATTERN.test(update.operation_id) &&
      Number.isSafeInteger(update.operation_revision) &&
      update.operation_revision >= 1 &&
      [
        "waiting_approval",
        "applying",
        "applied",
        "rejected",
        "failed",
        "unknown_execution",
      ].includes(update.status) &&
      update.artifact_source_revision === context.sourceRevision &&
      /^[0-9a-f]{64}$/.test(update.transaction_digest) &&
      changes.length >= 1 && changes.length <= mappings.length &&
      new Set(changes.map((change) => change.source_path)).size ===
        changes.length &&
      changes.every((change) =>
        validChange(change) &&
        expected.get(change.source_path) === change.target_path
      ) &&
      totalBytes <= 8 * 1024 * 1024 &&
      Boolean(first) && update.source_path === first.source_path &&
      update.target_path === first.target_path &&
      update.base_digest === first.base_digest &&
      update.proposed_digest === first.proposed_digest &&
      update.before === first.before && update.after === first.after,
  );
}

function validMappings(mappings: WorkspaceApplyMapping[]): boolean {
  return Array.isArray(mappings) && mappings.length >= 1 &&
    mappings.length <= 32 &&
    new Set(mappings.map((mapping) => mapping.source_path)).size ===
      mappings.length &&
    new Set(mappings.map((mapping) => mapping.target_path)).size ===
      mappings.length &&
    mappings.every((mapping) =>
      validVirtualPath(mapping.source_path) &&
      validTargetPath(mapping.target_path)
    );
}

function validChange(change: WorkspaceApplyChange): boolean {
  return Boolean(
    change && validVirtualPath(change.source_path) &&
      validTargetPath(change.target_path) &&
      (change.base_digest === null ||
        /^[0-9a-f]{64}$/.test(change.base_digest)) &&
      /^[0-9a-f]{64}$/.test(change.proposed_digest) &&
      typeof change.before === "string" &&
      typeof change.after === "string" &&
      new TextEncoder().encode(change.before).byteLength <= 1024 * 1024 &&
      new TextEncoder().encode(change.after).byteLength <= 1024 * 1024,
  );
}

function validVirtualPath(path: string): boolean {
  return typeof path === "string" && path.startsWith("/") &&
    path.length <= 4096 && !path.includes("..") && !path.includes("\\");
}

function validTargetPath(path: string): boolean {
  return typeof path === "string" && path.length >= 1 && path.length <= 4096 &&
    !path.startsWith("/") && !path.includes("\\") &&
    path.split("/").every((part) =>
      part.length > 0 && part !== "." && part !== ".." &&
      ![".git", ".hg", ".svn", ".jj"].includes(part)
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
