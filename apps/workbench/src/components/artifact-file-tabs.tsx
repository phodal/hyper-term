import { type KeyboardEvent, useRef } from "react";
import { nextHorizontalTabIndex } from "./roving-tab.ts";

interface ArtifactFileTabsProps {
  activePath: string;
  baselineFiles: Record<string, string>;
  entrypoint: string;
  files: Record<string, string>;
  onSelect(path: string): void;
}

export function ArtifactFileTabs({
  activePath,
  baselineFiles,
  entrypoint,
  files,
  onSelect,
}: ArtifactFileTabsProps) {
  const buttons = useRef<Array<HTMLButtonElement | null>>([]);
  const paths = Object.keys(files).sort((left, right) => {
    if (left === entrypoint) return -1;
    if (right === entrypoint) return 1;
    return left.localeCompare(right);
  });
  const onKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    currentIndex: number,
  ) => {
    const nextIndex = nextHorizontalTabIndex(
      paths.length,
      currentIndex,
      event.key,
    );
    if (nextIndex === undefined) return;
    event.preventDefault();
    onSelect(paths[nextIndex]);
    buttons.current[nextIndex]?.focus();
  };
  return (
    <nav
      className="artifact-file-tabs"
      aria-label="Artifact source files"
      role="tablist"
    >
      {paths.map((path, index) => {
        const changed = files[path] !== baselineFiles[path];
        return (
          <button
            key={path}
            ref={(button) => {
              buttons.current[index] = button;
            }}
            className={path === activePath ? "active" : ""}
            data-dirty={changed || undefined}
            type="button"
            role="tab"
            tabIndex={path === activePath ? 0 : -1}
            aria-selected={path === activePath}
            title={path}
            onClick={() => onSelect(path)}
            onKeyDown={(event) => onKeyDown(event, index)}
          >
            <span>{fileName(path)}</span>
            {path === entrypoint && <small>entry</small>}
            {changed && <i aria-label="modified" />}
          </button>
        );
      })}
    </nav>
  );
}

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1) || path;
}
