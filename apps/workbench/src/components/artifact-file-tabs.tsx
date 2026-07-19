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
  const paths = Object.keys(files).sort((left, right) => {
    if (left === entrypoint) return -1;
    if (right === entrypoint) return 1;
    return left.localeCompare(right);
  });
  return (
    <nav
      className="artifact-file-tabs"
      aria-label="Artifact source files"
      role="tablist"
    >
      {paths.map((path) => {
        const changed = files[path] !== baselineFiles[path];
        return (
          <button
            key={path}
            className={path === activePath ? "active" : ""}
            data-dirty={changed || undefined}
            type="button"
            role="tab"
            aria-selected={path === activePath}
            title={path}
            onClick={() => onSelect(path)}
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
