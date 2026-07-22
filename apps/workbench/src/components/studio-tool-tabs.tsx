import { type KeyboardEvent, useRef } from "react";
import type { ArtifactEditorView } from "../artifact-editor-checkpoint.ts";
import { nextHorizontalTabIndex } from "./roving-tab.ts";

interface StudioToolTabsProps {
  view: ArtifactEditorView;
  revision: number;
  languageStatus?: "idle" | "checking" | "ready" | "failed";
  onSelect(view: ArtifactEditorView): void;
}

const tools: readonly ArtifactEditorView[] = ["code", "diff", "trace"];

export function StudioToolTabs({
  view,
  revision,
  languageStatus,
  onSelect,
}: StudioToolTabsProps) {
  const buttons = useRef<Array<HTMLButtonElement | null>>([]);

  const onKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    currentIndex: number,
  ) => {
    const nextIndex = nextHorizontalTabIndex(
      tools.length,
      currentIndex,
      event.key,
    );
    if (nextIndex === undefined) return;
    event.preventDefault();
    onSelect(tools[nextIndex]);
    buttons.current[nextIndex]?.focus();
  };

  return (
    <div className="studio-tabs" role="tablist" aria-label="Artifact tools">
      {tools.map((tool, index) => (
        <button
          key={tool}
          ref={(button) => {
            buttons.current[index] = button;
          }}
          id={`artifact-${tool}-tab`}
          className={view === tool ? "active" : ""}
          data-view={tool}
          type="button"
          role="tab"
          tabIndex={view === tool ? 0 : -1}
          aria-controls={`artifact-${tool}-panel`}
          aria-selected={view === tool}
          onClick={() => onSelect(tool)}
          onKeyDown={(event) =>
            onKeyDown(event, index)}
        >
          {tool === "code" ? "Code" : tool === "diff" ? "Diff" : "Time Travel"}
        </button>
      ))}
      <span className="studio-spacer" />
      {languageStatus && (
        <span
          className={`language-status ${languageStatus}`}
          title="Rust-supervised Deno LSP against the private artifact snapshot"
          role="status"
          aria-live="polite"
        >
          Deno LSP · {languageStatus}
        </span>
      )}
      <span className="source-revision">source r{revision || 1}</span>
    </div>
  );
}
