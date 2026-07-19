import type {
  WorkspaceApplyStatus,
  WorkspaceApplyUpdate,
} from "../workspace-apply-publisher.ts";
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
  return (
    <section className="workspace-review" aria-label="Workspace apply review">
      <header className="workspace-review-header">
        <div>
          <span className="eyebrow">Brokered workspace diff</span>
          <strong>{review.source_path} → {review.target_path}</strong>
        </div>
        <span className="workspace-review-status" data-state={status}>
          {statusLabel(status)}
        </span>
        <button type="button" onClick={onBack}>Back to editor</button>
      </header>
      <div className="workspace-review-note" role="status">
        {status === "waiting_approval"
          ? "Review this Rust-captured diff, then approve the exact WorkspaceWrite operation in the Agent conversation."
          : status === "applied"
          ? "Rust rechecked the Artifact revision and workspace base, then installed this file atomically."
          : status === "unknown_execution"
          ? "The result could not be verified. Inspect the target before retrying."
          : "The approved transaction is being reconciled by Rust."}
      </div>
      <div className="workspace-review-diff">
        <CodeDiff
          key={review.operation_id}
          original={review.before}
          modified={review.after}
          onChange={ignoreReadOnlyChange}
          readOnlyModified
        />
      </div>
      <footer className="workspace-review-footer">
        <span>base {review.base_digest?.slice(0, 12) ?? "new file"}</span>
        <span>result {review.proposed_digest.slice(0, 12)}</span>
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
