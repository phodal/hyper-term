import type {
  WorkspaceApplyStatus,
  WorkspaceApplyUpdate,
} from "../workspace-apply-publisher.ts";
import { useState } from "react";
import { CodeDiff } from "./code-diff.tsx";

interface WorkspaceReviewProps {
  review: WorkspaceApplyUpdate;
  status: WorkspaceApplyStatus;
  error?: string;
  onBack(): void;
}

const ignoreReadOnlyChange = () => {};

export function WorkspaceReview({
  review,
  status,
  error,
  onBack,
}: WorkspaceReviewProps) {
  const [activeIndex, setActiveIndex] = useState(0);
  const active =
    review.changes[Math.min(activeIndex, review.changes.length - 1)];
  return (
    <section className="workspace-review" aria-label="Workspace apply review">
      <header className="workspace-review-header">
        <div>
          <span className="eyebrow">Brokered workspace diff</span>
          <strong>{review.changes.length} file transaction</strong>
        </div>
        <span className="workspace-review-status" data-state={status}>
          {statusLabel(status)}
        </span>
        <button type="button" onClick={onBack}>Back to editor</button>
      </header>
      <div className="workspace-review-note" role="status">
        {status === "waiting_approval"
          ? "Review every Rust-captured file diff, then approve this exact WorkspaceWrite set in the Agent conversation."
          : status === "applied"
          ? "Rust rechecked every Artifact source and workspace base, then installed the set with guarded rollback."
          : status === "unknown_execution"
          ? "The result could not be verified. Inspect the target before retrying."
          : "The approved transaction is being reconciled by Rust."}
      </div>
      <nav className="workspace-review-files" aria-label="Workspace changes">
        {review.changes.map((change, index) => (
          <button
            type="button"
            aria-current={index === activeIndex ? "true" : undefined}
            className={index === activeIndex ? "active" : ""}
            key={change.source_path}
            onClick={() => setActiveIndex(index)}
            title={`${change.source_path} → ${change.target_path}`}
          >
            <span>{change.source_path.slice(1)}</span>
            <small>{change.target_path}</small>
          </button>
        ))}
      </nav>
      <div className="workspace-review-diff">
        <CodeDiff
          key={`${review.operation_id}:${active.source_path}`}
          original={active.before}
          modified={active.after}
          onChange={ignoreReadOnlyChange}
          readOnlyModified
        />
      </div>
      <footer className="workspace-review-footer">
        <span>{active.source_path} → {active.target_path}</span>
        <span>base {active.base_digest?.slice(0, 12) ?? "new file"}</span>
        <span>set {review.transaction_digest.slice(0, 12)}</span>
        {error && <strong role="alert">{error}</strong>}
      </footer>
    </section>
  );
}

function statusLabel(status: WorkspaceApplyStatus): string {
  switch (status) {
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
