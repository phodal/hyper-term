import { useState } from "react";
import type { WorkspaceApplyMapping } from "../workspace-apply-publisher.ts";

interface WorkspaceApplyComposerProps {
  sourcePaths: string[];
  busy: boolean;
  error?: string;
  onCancel(): void;
  onReview(mappings: WorkspaceApplyMapping[]): void;
}

interface MappingDraft extends WorkspaceApplyMapping {
  selected: boolean;
}

const maximumFiles = 32;

export function WorkspaceApplyComposer({
  sourcePaths,
  busy,
  error,
  onCancel,
  onReview,
}: WorkspaceApplyComposerProps) {
  const [drafts, setDrafts] = useState<MappingDraft[]>(() =>
    [...sourcePaths].sort().map((sourcePath, index) => ({
      source_path: sourcePath,
      target_path: sourcePath.slice(1),
      selected: index < maximumFiles,
    }))
  );
  const selected = drafts.filter((draft) => draft.selected);
  const validation = validateMappings(selected);

  const updateDraft = (
    sourcePath: string,
    update: Partial<Pick<MappingDraft, "selected" | "target_path">>,
  ) => {
    setDrafts((current) =>
      current.map((draft) =>
        draft.source_path === sourcePath ? { ...draft, ...update } : draft
      )
    );
  };

  return (
    <section className="workspace-mapping" aria-label="Map Artifact files">
      <header className="workspace-mapping-header">
        <div>
          <span className="eyebrow">Brokered file-set apply</span>
          <strong>Map Artifact source to workspace targets</strong>
        </div>
        <button type="button" onClick={onCancel}>Back to editor</button>
      </header>
      <div className="workspace-mapping-note">
        Select up to {maximumFiles}{" "}
        bounded text files. Rust captures each target base and computes
        selectable hunks without creating a WorkspaceWrite approval.
      </div>
      <form
        className="workspace-mapping-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (busy || validation) return;
          onReview(selected.map(({ source_path, target_path }) => ({
            source_path,
            target_path: target_path.trim(),
          })));
        }}
      >
        <div className="workspace-mapping-list">
          {drafts.map((draft) => (
            <label
              className="workspace-mapping-row"
              data-selected={draft.selected || undefined}
              key={draft.source_path}
            >
              <input
                type="checkbox"
                checked={draft.selected}
                disabled={busy ||
                  !draft.selected && selected.length >= maximumFiles}
                onChange={(event) =>
                  updateDraft(draft.source_path, {
                    selected: event.target.checked,
                  })}
              />
              <span title={draft.source_path}>{draft.source_path}</span>
              <b aria-hidden="true">→</b>
              <input
                aria-label={`Workspace target for ${draft.source_path}`}
                autoComplete="off"
                disabled={busy || !draft.selected}
                maxLength={4096}
                spellCheck={false}
                value={draft.target_path}
                onChange={(event) =>
                  updateDraft(draft.source_path, {
                    target_path: event.target.value,
                  })}
              />
            </label>
          ))}
        </div>
        <footer className="workspace-mapping-footer">
          <span>{selected.length} selected · preview first</span>
          {(validation || error) && (
            <strong role="alert">{validation ?? error}</strong>
          )}
          <button type="submit" disabled={busy || Boolean(validation)}>
            {busy ? "Capturing bases" : `Review ${selected.length} file(s)`}
          </button>
        </footer>
      </form>
    </section>
  );
}

function validateMappings(drafts: MappingDraft[]): string | undefined {
  if (drafts.length === 0) return "Select at least one Artifact file.";
  if (drafts.length > maximumFiles) return "Select no more than 32 files.";
  const targets = drafts.map((draft) => draft.target_path.trim());
  if (targets.some((target) => !validTargetPath(target))) {
    return "Targets must be safe workspace-relative paths.";
  }
  if (new Set(targets).size !== targets.length) {
    return "Each selected source needs a unique workspace target.";
  }
  return undefined;
}

function validTargetPath(path: string): boolean {
  return path.length >= 1 && path.length <= 4096 && !path.startsWith("/") &&
    !path.includes("\\") &&
    path.split("/").every((part) =>
      part.length > 0 && part !== "." && part !== ".." &&
      ![".git", ".hg", ".svn", ".jj"].includes(part)
    );
}
