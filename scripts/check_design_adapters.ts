import { lint } from "designmd/linter";

export interface DesignAdapterSources {
  nativeZig: string;
  terminalCss: string;
  terminalTs: string;
  workbenchCss: string;
  previewHtml: string;
}

interface ColorToken {
  type: "color";
  hex: string;
  r: number;
  g: number;
  b: number;
}

interface DimensionToken {
  type: "dimension";
  unit: string;
  value: number;
}

interface TypographyToken {
  type: "typography";
  fontSize?: DimensionToken;
}

type TokenMap = Map<string, unknown>;

const sourceLabels: Record<keyof DesignAdapterSources, string> = {
  nativeZig: "apps/desktop/src/main.zig",
  terminalCss: "apps/terminal/src/styles.css",
  terminalTs: "apps/terminal/src/terminal-theme.ts",
  workbenchCss: "apps/workbench/src/styles.css",
  previewHtml: "apps/workbench/genui/preview.html",
};

function requiredToken<T>(
  tokens: TokenMap,
  name: string,
  type: string,
): T {
  const token = tokens.get(name) as { type?: string } | undefined;
  if (!token || token.type !== type) {
    throw new Error(`DESIGN.md is missing the required ${type} token ${name}`);
  }
  return token as T;
}

function requiredColor(tokens: TokenMap, name: string): ColorToken {
  return requiredToken<ColorToken>(tokens, name, "color");
}

function requiredDimension(tokens: TokenMap, name: string): DimensionToken {
  const token = requiredToken<DimensionToken>(tokens, name, "dimension");
  if (token.unit !== "px") {
    throw new Error(`DESIGN.md token ${name} must use px for Native SDK`);
  }
  return token;
}

function requiredFontSize(tokens: TokenMap, name: string): number {
  const token = requiredToken<TypographyToken>(tokens, name, "typography");
  if (!token.fontSize || token.fontSize.unit !== "px") {
    throw new Error(
      `DESIGN.md typography token ${name} must declare a px fontSize`,
    );
  }
  return token.fontSize.value;
}

function zigRgb(color: ColorToken): string {
  return `canvas.Color.rgb8(${color.r}, ${color.g}, ${color.b})`;
}

function recordMissing(
  findings: string[],
  sources: DesignAdapterSources,
  source: keyof DesignAdapterSources,
  description: string,
  expected: string,
): void {
  if (!sources[source].includes(expected)) {
    findings.push(
      `${
        sourceLabels[source]
      } does not adapt ${description}: expected ${expected}`,
    );
  }
}

export function collectDesignAdapterFindings(
  designSource: string,
  sources: DesignAdapterSources,
): string[] {
  const report = lint(designSource);
  const lintErrors = report.findings
    .filter((finding) => finding.severity === "error")
    .map((finding) => `DESIGN.md: ${finding.message}`);
  if (lintErrors.length > 0) return lintErrors;

  const design = report.designSystem;
  const colors = design.colors as TokenMap;
  const typography = design.typography as TokenMap;
  const spacing = design.spacing as TokenMap;
  const rounded = design.rounded as TokenMap;
  const findings: string[] = [];

  const nativeColorRoles = [
    ["background", "background"],
    ["surface", "surface"],
    ["surface_subtle", "surface-subtle"],
    ["surface_pressed", "surface-pressed"],
    ["text", "text"],
    ["text_muted", "text-muted"],
    ["border", "border"],
    ["accent", "accent"],
    ["accent_text", "accent-text"],
    ["destructive", "destructive"],
    ["success", "success"],
    ["warning", "warning"],
    ["info", "info"],
    ["focus_ring", "focus"],
  ] as const;
  for (const scheme of ["light", "dark"] as const) {
    for (const [nativeRole, designRole] of nativeColorRoles) {
      const color = requiredColor(colors, `${designRole}-${scheme}`);
      recordMissing(
        findings,
        sources,
        "nativeZig",
        `colors.${designRole}-${scheme}`,
        `.${nativeRole} = ${zigRgb(color)}`,
      );
    }
  }

  const typographyRoles = ["body", "label", "title", "heading", "display"];
  for (const role of typographyRoles) {
    const size = requiredFontSize(typography, role);
    recordMissing(
      findings,
      sources,
      "nativeZig",
      `typography.${role}.fontSize`,
      `tokens.typography.${role}_size = ${size};`,
    );
  }

  const spacingValues = ["xs", "sm", "md", "lg", "xl"].map((name) =>
    requiredDimension(spacing, name).value
  );
  recordMissing(
    findings,
    sources,
    "nativeZig",
    "spacing scale",
    `tokens.spacing = .{ .xs = ${spacingValues[0]}, .sm = ${
      spacingValues[1]
    }, .md = ${spacingValues[2]}, .lg = ${spacingValues[3]}, .xl = ${
      spacingValues[4]
    } };`,
  );

  const radiusValues = ["sm", "md", "lg", "xl"].map((name) =>
    requiredDimension(rounded, name).value
  );
  recordMissing(
    findings,
    sources,
    "nativeZig",
    "rounded scale",
    `tokens.radius = .{ .sm = ${radiusValues[0]}, .md = ${
      radiusValues[1]
    }, .lg = ${radiusValues[2]}, .xl = ${radiusValues[3]} };`,
  );

  const workbenchCssRoles = [
    ["background", "--workbench-background"],
    ["surface", "--workbench-surface"],
    ["surface-subtle", "--workbench-surface-subtle"],
    ["surface-pressed", "--workbench-surface-pressed"],
    ["text", "--workbench-text"],
    ["text-muted", "--workbench-text-muted"],
    ["border", "--workbench-border"],
    ["accent", "--workbench-accent"],
    ["accent-text", "--workbench-accent-text"],
    ["focus", "--workbench-focus"],
    ["success", "--workbench-success"],
    ["warning", "--workbench-warning"],
    ["destructive", "--workbench-destructive"],
    ["info", "--workbench-info"],
  ] as const;
  for (const scheme of ["dark", "light"] as const) {
    for (const [designRole, property] of workbenchCssRoles) {
      const tokenName = `${designRole}-${scheme}`;
      const color = requiredColor(colors, tokenName);
      recordMissing(
        findings,
        sources,
        "workbenchCss",
        `colors.${tokenName}`,
        `${property}: ${color.hex.toLowerCase()};`,
      );
    }
  }

  const previewRoles = [
    ["background", "--preview-background"],
    ["text", "--preview-text"],
    ["destructive", "--preview-error-text"],
  ] as const;
  for (const scheme of ["dark", "light"] as const) {
    for (const [designRole, property] of previewRoles) {
      const tokenName = `${designRole}-${scheme}`;
      const color = requiredColor(colors, tokenName);
      recordMissing(
        findings,
        sources,
        "previewHtml",
        `colors.${tokenName}`,
        `${property}: ${color.hex.toLowerCase()};`,
      );
    }
  }

  const terminalCssRoles = [
    [
      "terminalCss",
      "dark background",
      "--terminal-background",
      "background-dark",
    ],
    ["terminalCss", "dark surface", "--terminal-surface", "surface-dark"],
    [
      "terminalCss",
      "dark subtle surface",
      "--terminal-surface-subtle",
      "surface-subtle-dark",
    ],
    [
      "terminalCss",
      "dark pressed surface",
      "--terminal-surface-pressed",
      "surface-pressed-dark",
    ],
    ["terminalCss", "dark text", "--terminal-text", "text-dark"],
    [
      "terminalCss",
      "dark muted text",
      "--terminal-text-muted",
      "text-muted-dark",
    ],
    ["terminalCss", "dark border", "--terminal-border", "border-dark"],
    ["terminalCss", "dark accent", "--terminal-accent", "accent-dark"],
    ["terminalCss", "dark focus", "--terminal-focus", "focus-dark"],
    [
      "terminalCss",
      "light background",
      "--terminal-background",
      "background-light",
    ],
    ["terminalCss", "light surface", "--terminal-surface", "surface-light"],
    [
      "terminalCss",
      "light subtle surface",
      "--terminal-surface-subtle",
      "surface-subtle-light",
    ],
    [
      "terminalCss",
      "light pressed surface",
      "--terminal-surface-pressed",
      "surface-pressed-light",
    ],
    ["terminalCss", "light text", "--terminal-text", "text-light"],
    [
      "terminalCss",
      "light muted text",
      "--terminal-text-muted",
      "text-muted-light",
    ],
    ["terminalCss", "light border", "--terminal-border", "border-light"],
    ["terminalCss", "light accent", "--terminal-accent", "accent-light"],
    ["terminalCss", "light focus", "--terminal-focus", "focus-light"],
  ] as const;
  for (const [source, description, property, tokenName] of terminalCssRoles) {
    const color = requiredColor(colors, tokenName);
    recordMissing(
      findings,
      sources,
      source,
      `colors.${tokenName} (${description})`,
      `${property}: ${color.hex.toLowerCase()};`,
    );
  }

  const terminalThemeRoles = [
    ["background", "background"],
    ["foreground", "text"],
    ["cursor", "accent"],
    ["cursorAccent", "accent-text"],
    ["red", "destructive"],
    ["green", "success"],
    ["yellow", "warning"],
    ["blue", "info"],
    ["white", "text"],
    ["brightBlack", "text-muted"],
    ["brightGreen", "accent"],
  ] as const;
  for (const scheme of ["dark", "light"] as const) {
    for (const [terminalRole, designRole] of terminalThemeRoles) {
      const tokenName = `${designRole}-${scheme}`;
      const color = requiredColor(colors, tokenName);
      recordMissing(
        findings,
        sources,
        "terminalTs",
        `colors.${tokenName}`,
        `${terminalRole}: "${color.hex.toLowerCase()}",`,
      );
    }
  }

  return findings;
}

async function readSources(): Promise<DesignAdapterSources> {
  const [nativeZig, terminalCss, terminalTs, workbenchCss, previewHtml] =
    await Promise.all([
      Deno.readTextFile("apps/desktop/src/main.zig"),
      Deno.readTextFile("apps/terminal/src/styles.css"),
      Deno.readTextFile("apps/terminal/src/terminal-theme.ts"),
      Deno.readTextFile("apps/workbench/src/styles.css"),
      Deno.readTextFile("apps/workbench/genui/preview.html"),
    ]);
  return { nativeZig, terminalCss, terminalTs, workbenchCss, previewHtml };
}

if (import.meta.main) {
  const designSource = await Deno.readTextFile("DESIGN.md");
  const findings = collectDesignAdapterFindings(
    designSource,
    await readSources(),
  );
  if (findings.length > 0) {
    for (const finding of findings) {
      console.error(`design adapter drift: ${finding}`);
    }
    Deno.exit(1);
  }
  console.log("Design adapters match the normative DESIGN.md tokens.");
}
