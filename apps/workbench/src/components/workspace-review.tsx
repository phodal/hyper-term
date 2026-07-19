import { useState } from "react";
import type {
  WorkspaceApplyPreview,
  WorkspaceApplyStatus,
  WorkspaceApplyUpdate,
  WorkspaceHunkSelection,
} from "../workspace-apply-publisher.ts";
import { CodeDiff } from "./code-diff.tsx";

interface WorkspaceReviewProps {
  preview: WorkspaceApplyPreview;
  review?: WorkspaceApplyUpdate;
  busy: boolean;
  error?: string;
  onBack(): void;
  onApply(selections: WorkspaceHunkSelection[]): void;
}

type ReviewDisplayStatus = WorkspaceApplyStatus | "reviewing" | "preparing";

const ignoreReadOnlyChange = () => {};

export function WorkspaceReview({
  preview,
  review,
  busy,
  error,
  onBack,
  onApply,
}: WorkspaceReviewProps) {
  const [activeSourcePath, setActiveSourcePath] = useState(
    preview.changes[0].source_path,
  );
  const [selectedHunks, setSelectedHunks] = useState<Record<string, string[]>>(
    () =>
      Object.fromEntries(
        preview.changes.map((change) => [
          change.source_path,
          change.hunks.map((hunk) => hunk.id),
        ]),
      ),
  );
  const active =
    preview.changes.find((change) => change.source_path === activeSourcePath) ??
      preview.changes[0];
  const exactChange = review?.changes.find((change) =>
    change.source_path === active.source_path
  );
  const status: ReviewDisplayStatus = review?.status ??
    (busy ? "preparing" : "reviewing");
  const locked = busy || Boolean(review);
  const selectedCount = Object.values(selectedHunks).reduce(
    (count, hunkIds) => count + hunkIds.length,
    0,
  );
  const selectedFileCount = Object.values(selectedHunks).filter((hunkIds) =>
    hunkIds.length > 0
  ).length;
  const activeSelection = selectedHunks[active.source_path] ?? [];
  const allActiveSelected = activeSelection.length === active.hunks.length;

  const setHunkSelected = (hunkId: string, selected: boolean) => {
    if (locked) return;
    setSelectedHunks((current) => {
      const next = new Set(current[active.source_path] ?? []);
      if (selected) next.add(hunkId);
      else next.delete(hunkId);
      return { ...current, [active.source_path]: [...next] };
    });
  };

  const selections = (): WorkspaceHunkSelection[] =>
    preview.changes.map((change) => ({
      source_path: change.source_path,
      target_path: change.target_path,
      hunk_ids: selectedHunks[change.source_path] ?? [],
    }));

  return (
    <section className="workspace-review" aria-label="Workspace hunk review">
      <header className="workspace-review-header">
        <div>
          <span className="eyebrow">Rust-brokered hunk review</span>
          <strong>
            {selectedCount} hunk(s) · {selectedFileCount} file transaction
          </strong>
        </div>
        <span className="workspace-review-status" data-state={status}>
          {statusLabel(status)}
        </span>
        <button type="button" onClick={onBack}>Back to editor</button>
      </header>
      <div className="workspace-review-note" role="status">
        {statusNote(status)}
      </div>
      <nav className="workspace-review-files" aria-label="Workspace changes">
        {preview.changes.map((change) => {
          const fileSelection = selectedHunks[change.source_path] ?? [];
          return (
            <button
              type="button"
              aria-current={change.source_path === active.source_path
                ? "true"
                : undefined}
              className={change.source_path === active.source_path
                ? "active"
                : ""}
              key={change.source_path}
              onClick={() => setActiveSourcePath(change.source_path)}
              title={`${change.source_path} → ${change.target_path}`}
            >
              <span>{change.source_path.slice(1)}</span>
              <small>
                {fileSelection.length}/{change.hunks.length} ·{" "}
                {change.target_path}
              </small>
            </button>
          );
        })}
      </nav>
      <div className="workspace-review-body">
        <fieldset className="workspace-hunk-list" disabled={locked}>
          <legend>
            <span>Selectable hunks</span>
            <label>
              <input
                type="checkbox"
                checked={allActiveSelected}
                onChange={(event) =>
                  setSelectedHunks((current) => ({
                    ...current,
                    [active.source_path]: event.target.checked
                      ? active.hunks.map((hunk) =>
                        hunk.id
                      )
                      : [],
                  }))}
              />
              All in file
            </label>
          </legend>
          <div>
            {active.hunks.map((hunk, index) => (
              <label
                className="workspace-hunk"
                data-selected={activeSelection.includes(hunk.id) || undefined}
                key={hunk.id}
              >
                <span className="workspace-hunk-heading">
                  <input
                    type="checkbox"
                    checked={activeSelection.includes(hunk.id)}
                    onChange={(event) =>
                      setHunkSelected(hunk.id, event.target.checked)}
                  />
                  <b>Hunk {index + 1}</b>
                  <small>
                    −{hunk.base_start},{hunk.base_lines}{" "}
                    +{hunk.proposed_start},{hunk.proposed_lines}
                  </small>
                </span>
                <pre>{hunk.patch}</pre>
              </label>
            ))}
          </div>
        </fieldset>
        <div className="workspace-review-diff">
          <div className="workspace-review-diff-label">
            <span>
              {exactChange
                ? "Exact brokered result"
                : "Artifact candidate · selections are listed at left"}
            </span>
            {!exactChange && activeSelection.length === 0 && (
              <small>This file will not enter the approval.</small>
            )}
          </div>
          <CodeDiff
            key={`${
              review?.operation_id ?? preview.review_digest
            }:${active.source_path}`}
            original={active.before}
            modified={exactChange?.after ?? active.artifact_after}
            onChange={ignoreReadOnlyChange}
            readOnlyModified
          />
        </div>
      </div>
      <footer className="workspace-review-footer">
        <span>{active.source_path} → {active.target_path}</span>
        <span>base {active.base_digest?.slice(0, 12) ?? "new file"}</span>
        {review && <span>set {review.transaction_digest.slice(0, 12)}</span>}
        {error && <strong role="alert">{error}</strong>}
        {!review && (
          <button
            type="button"
            disabled={busy || selectedCount === 0}
            onClick={() => onApply(selections())}
          >
            {busy
              ? "Creating exact approval"
              : `Create approval for ${selectedCount} hunk(s)`}
          </button>
        )}
      </footer>
    </section>
  );
}

function statusLabel(status: ReviewDisplayStatus): string {
  switch (status) {
    case "reviewing":
      return "Select hunks";
    case "preparing":
      return "Binding selection";
    case "waiting_approval":
      return "Waiting for approval";
    case "applying":
      return "Applying";
    case "applied":
      return "Applied";
    case "rejected":
      return "Rejected";
    case "unknown_execution":
      return "Inspect target";
    case "failed":
      return "Failed safely";
  }
}

function statusNote(status: ReviewDisplayStatus): string {
  switch (status) {
    case "reviewing":
      return "Select the Rust-computed hunks to keep. Creating an approval does not write files; the exact reconstructed diff appears before you approve in Agent.";
    case "preparing":
      return "Rust is validating the hunk IDs against this review and binding the exact reconstructed file set.";
    case "waiting_approval":
      return "The diff now shows the exact selected result. Approve this one WorkspaceWrite operation in the Agent conversation.";
    case "applying":
      return "Rust is rechecking the Artifact sources and every captured workspace base before installing the set.";
    case "applied":
      return "Rust installed the selected set transactionally with guarded rollback.";
    case "rejected":
      return "No workspace files were written. Start a new review if you want a different selection.";
    case "failed":
      return "The bounded apply failed without a verified success. Review the error before retrying.";
    case "unknown_execution":
      return "The result could not be verified. Inspect the targets before retrying.";
  }
}
