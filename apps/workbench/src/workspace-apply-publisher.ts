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

export interface WorkspaceDiffHunk {
  id: string;
  base_start: number;
  base_lines: number;
  proposed_start: number;
  proposed_lines: number;
  patch: string;
}

export interface WorkspacePreviewChange extends WorkspaceApplyMapping {
  base_digest: string | null;
  artifact_digest: string;
  before: string;
  artifact_after: string;
  hunks: WorkspaceDiffHunk[];
}

export interface WorkspaceApplyPreview {
  artifact_source_revision: number;
  review_digest: string;
  changes: WorkspacePreviewChange[];
}

export interface WorkspaceHunkSelection extends WorkspaceApplyMapping {
  hunk_ids: string[];
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
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const MAX_FILE_BYTES = 1024 * 1024;
const MAX_PREVIEW_BYTES = 16 * 1024 * 1024;
const MAX_HUNKS_PER_FILE = 256;

export class WorkspaceApplyPublisher {
  constructor(
    private readonly context: WorkspaceApplyPublisherContext,
    private readonly fetcher: Fetcher = (input, init) =>
      globalThis.fetch(input, init),
    private readonly sleeper: Sleeper = abortableDelay,
  ) {}

  async preview(
    mappings: WorkspaceApplyMapping[],
    signal: AbortSignal,
  ): Promise<WorkspaceApplyPreview> {
    if (!validMappings(mappings)) {
      throw new Error("Workspace apply mappings are invalid or ambiguous.");
    }
    const exactMappings = mappings.map((mapping) => ({ ...mapping }));
    const response = await this.fetcher(
      this.#endpoint("workspace-preview"),
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          artifact_source_revision: this.context.sourceRevision,
          mappings: exactMappings,
        }),
        signal,
      },
    );
    if (!response.ok) {
      const detail = (await response.text()).trim();
      throw new Error(
        detail ||
          `Rust workspace preview endpoint returned ${response.status}.`,
      );
    }
    const payload = await response.json() as WorkspaceApplyPreview;
    if (!validPreview(payload, this.context, exactMappings)) {
      throw new Error(
        "Rust workspace preview did not match the editor context.",
      );
    }
    return payload;
  }

  async apply(
    preview: WorkspaceApplyPreview,
    selections: WorkspaceHunkSelection[],
    onUpdate: (update: WorkspaceApplyUpdate) => void,
    signal: AbortSignal,
  ): Promise<WorkspaceApplyUpdate> {
    const previewMappings =
      preview?.changes?.map(({ source_path, target_path }) => ({
        source_path,
        target_path,
      })) ?? [];
    if (
      !validPreview(preview, this.context, previewMappings) ||
      !validSelections(preview, selections)
    ) {
      throw new Error("Workspace hunk selection is invalid or stale.");
    }
    const exactSelections = selections.map((selection) => ({
      ...selection,
      hunk_ids: [...selection.hunk_ids],
    }));
    const selectedMappings = exactSelections
      .filter((selection) => selection.hunk_ids.length > 0)
      .map(({ source_path, target_path }) => ({ source_path, target_path }));
    let update = await this.#requestApply(
      "POST",
      exactSelections,
      selectedMappings,
      preview.review_digest,
      undefined,
      signal,
    );
    onUpdate(update);
    while (
      update.status === "waiting_approval" || update.status === "applying"
    ) {
      await this.sleeper(350, signal);
      update = await this.#requestApply(
        "GET",
        exactSelections,
        selectedMappings,
        preview.review_digest,
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

  async #requestApply(
    method: "GET" | "POST",
    selections: WorkspaceHunkSelection[],
    selectedMappings: WorkspaceApplyMapping[],
    reviewDigest: string,
    operationId: string | undefined,
    signal: AbortSignal,
  ): Promise<WorkspaceApplyUpdate> {
    const response = await this.fetcher(
      this.#endpoint("workspace-apply", operationId),
      {
        method,
        cache: "no-store",
        ...(method === "POST"
          ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              artifact_source_revision: this.context.sourceRevision,
              review_digest: reviewDigest,
              mappings: selections,
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
    if (!validUpdate(payload, this.context, selectedMappings)) {
      throw new Error(
        "Rust workspace apply response did not match the editor context.",
      );
    }
    return payload;
  }

  #endpoint(
    kind: "workspace-preview" | "workspace-apply",
    operationId?: string,
  ) {
    const query = new URLSearchParams({
      token: this.context.token,
      session_id: String(this.context.sessionId),
      ...(operationId ? { operation_id: operationId } : {}),
    });
    return `/agent/artifact/${
      encodeURIComponent(this.context.artifactId)
    }/${kind}?${query}`;
  }
}

function validPreview(
  preview: WorkspaceApplyPreview,
  context: WorkspaceApplyPublisherContext,
  mappings: WorkspaceApplyMapping[],
): boolean {
  if (!validMappings(mappings)) return false;
  const expected = new Map(
    mappings.map((mapping) => [mapping.source_path, mapping.target_path]),
  );
  const changes = Array.isArray(preview?.changes) ? preview.changes : [];
  let totalBytes = 0;
  for (const change of changes) {
    if (!validPreviewChange(change)) return false;
    totalBytes += byteLength(change.before) + byteLength(change.artifact_after);
    totalBytes += change.hunks.reduce(
      (total, hunk) => total + byteLength(hunk.patch),
      0,
    );
  }
  return Boolean(
    preview && preview.artifact_source_revision === context.sourceRevision &&
      SHA256_PATTERN.test(preview.review_digest) && changes.length >= 1 &&
      changes.length <= mappings.length &&
      new Set(changes.map((change) => change.source_path)).size ===
        changes.length &&
      changes.every((change) =>
        expected.get(change.source_path) === change.target_path
      ) && totalBytes <= MAX_PREVIEW_BYTES,
  );
}

function validSelections(
  preview: WorkspaceApplyPreview,
  selections: WorkspaceHunkSelection[],
): boolean {
  if (
    !Array.isArray(selections) || selections.length !== preview.changes.length
  ) {
    return false;
  }
  const selectionBySource = new Map(
    selections.map((selection) => [selection.source_path, selection]),
  );
  if (selectionBySource.size !== selections.length) return false;
  let selectedCount = 0;
  for (const change of preview.changes) {
    const selection = selectionBySource.get(change.source_path);
    if (
      !selection || selection.target_path !== change.target_path ||
      !Array.isArray(selection.hunk_ids) ||
      selection.hunk_ids.length > MAX_HUNKS_PER_FILE ||
      new Set(selection.hunk_ids).size !== selection.hunk_ids.length
    ) {
      return false;
    }
    const available = new Set(change.hunks.map((hunk) => hunk.id));
    if (selection.hunk_ids.some((id) => !available.has(id))) return false;
    selectedCount += selection.hunk_ids.length;
  }
  return selectedCount > 0;
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
      total + byteLength(change.before) +
      byteLength(change.after),
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
      SHA256_PATTERN.test(update.transaction_digest) &&
      changes.length === mappings.length &&
      new Set(changes.map((change) => change.source_path)).size ===
        changes.length &&
      changes.every((change) =>
        validChange(change) &&
        expected.get(change.source_path) === change.target_path
      ) && totalBytes <= 8 * 1024 * 1024 && Boolean(first) &&
      update.source_path === first.source_path &&
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

function validPreviewChange(change: WorkspacePreviewChange): boolean {
  const hunks = Array.isArray(change?.hunks) ? change.hunks : [];
  return Boolean(
    change && validVirtualPath(change.source_path) &&
      validTargetPath(change.target_path) &&
      (change.base_digest === null ||
        SHA256_PATTERN.test(change.base_digest)) &&
      SHA256_PATTERN.test(change.artifact_digest) &&
      typeof change.before === "string" &&
      typeof change.artifact_after === "string" &&
      byteLength(change.before) <= MAX_FILE_BYTES &&
      byteLength(change.artifact_after) <= MAX_FILE_BYTES &&
      hunks.length >= 1 &&
      hunks.length <= MAX_HUNKS_PER_FILE &&
      new Set(hunks.map((hunk) => hunk.id)).size === hunks.length &&
      hunks.every(validHunk),
  );
}

function validHunk(hunk: WorkspaceDiffHunk): boolean {
  return Boolean(
    hunk && SHA256_PATTERN.test(hunk.id) &&
      Number.isSafeInteger(hunk.base_start) && hunk.base_start >= 0 &&
      Number.isSafeInteger(hunk.base_lines) && hunk.base_lines >= 0 &&
      Number.isSafeInteger(hunk.proposed_start) && hunk.proposed_start >= 0 &&
      Number.isSafeInteger(hunk.proposed_lines) && hunk.proposed_lines >= 0 &&
      hunk.base_lines + hunk.proposed_lines > 0 &&
      typeof hunk.patch === "string" && hunk.patch.startsWith("@@ ") &&
      byteLength(hunk.patch) <= MAX_FILE_BYTES,
  );
}

function validChange(change: WorkspaceApplyChange): boolean {
  return Boolean(
    change && validVirtualPath(change.source_path) &&
      validTargetPath(change.target_path) &&
      (change.base_digest === null ||
        SHA256_PATTERN.test(change.base_digest)) &&
      SHA256_PATTERN.test(change.proposed_digest) &&
      typeof change.before === "string" && typeof change.after === "string" &&
      byteLength(change.before) <= MAX_FILE_BYTES &&
      byteLength(change.after) <= MAX_FILE_BYTES,
  );
}

function byteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
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
