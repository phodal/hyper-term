export type WorkbenchColorScheme = "dark" | "light";

export function workbenchColorScheme(
  prefersLight: boolean,
): WorkbenchColorScheme {
  return prefersLight ? "light" : "dark";
}

export function applyWorkbenchColorScheme(
  root: Pick<HTMLElement, "dataset">,
  scheme: WorkbenchColorScheme,
): void {
  root.dataset.theme = scheme;
}
