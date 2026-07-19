export type WorkspaceApplyStatus =
  | "waiting_approval"
  | "applying"
  | "applied"
  | "rejected"
  | "failed"
  | "unknown_execution";

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
  error?: string;
}

export interface WorkspaceApplyPublisherContext {
  artifactId: string;
  sourceRevision: number;
  sourcePath: string;
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
    targetPath: string,
    onUpdate: (update: WorkspaceApplyUpdate) => void,
    signal: AbortSignal,
  ): Promise<WorkspaceApplyUpdate> {
    let update = await this.#request("POST", targetPath, undefined, signal);
    onUpdate(update);
    while (
      update.status === "waiting_approval" || update.status === "applying"
    ) {
      await this.sleeper(350, signal);
      update = await this.#request(
        "GET",
        targetPath,
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
    targetPath: string,
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
              source_path: this.context.sourcePath,
              target_path: targetPath,
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
    if (!validUpdate(payload, this.context, targetPath)) {
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
  targetPath: string,
): boolean {
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
      update.source_path === context.sourcePath &&
      update.target_path === targetPath &&
      (update.base_digest === null ||
        /^[0-9a-f]{64}$/.test(update.base_digest)) &&
      /^[0-9a-f]{64}$/.test(update.proposed_digest) &&
      typeof update.before === "string" &&
      typeof update.after === "string" &&
      update.before.length <= 1024 * 1024 &&
      update.after.length <= 1024 * 1024,
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
