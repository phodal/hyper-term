export type VisualFindingCategory =
  | "empty_render"
  | "viewport_overflow"
  | "clipped_content"
  | "undersized_target"
  | "low_contrast"
  | "hidden_primary_action"
  | "console_error"
  | "resource_failure"
  | "layout_instability";

export interface VisualQualityMeasureRequest {
  capture_id: string;
  viewport: { width: number; height: number };
  color_scheme: "light" | "dark";
  locale: "en";
  scenario: "default";
  reduced_motion: boolean;
}

export interface VisualQualityRuntimeCounters {
  consoleErrors: number;
  resourceFailures: number;
  layoutShiftMilli: number;
}

interface VisualIssueSample {
  category: VisualFindingCategory;
  semantic_path: string;
  rect?: { x: number; y: number; width: number; height: number };
}

type VisualRect = NonNullable<VisualIssueSample["rect"]>;

export interface VisualCaptureObservation {
  capture_id: string;
  viewport: { width: number; height: number };
  color_scheme: "light" | "dark";
  locale: "en";
  scenario: "default";
  reduced_motion: boolean;
  document_width: number;
  document_height: number;
  element_count: number;
  interactive_count: number;
  clipped_count: number;
  undersized_target_count: number;
  low_contrast_count: number;
  hidden_primary_action_count: number;
  console_error_count: number;
  resource_failure_count: number;
  layout_shift_milli: number;
  semantic_digest: string;
  samples: VisualIssueSample[];
}

const INTERACTIVE_SELECTOR =
  "button,a[href],input,select,textarea,[role=button],[tabindex]";
const PRIMARY_SELECTOR = '[data-primary-action="true"]';
const MAX_SAMPLES = 24;

export async function measureVisualQuality(
  root: HTMLElement,
  request: VisualQualityMeasureRequest,
  counters: VisualQualityRuntimeCounters,
): Promise<VisualCaptureObservation> {
  await settleLayout(request.viewport);
  const viewport = {
    width: Math.max(1, Math.round(globalThis.innerWidth)),
    height: Math.max(1, Math.round(globalThis.innerHeight)),
  };
  const elements = [root, ...root.querySelectorAll<HTMLElement>("*")]
    .filter((element): element is HTMLElement =>
      element instanceof HTMLElement
    );
  const visible = elements.filter(isVisible);
  const interactive = visible.filter((element) =>
    element.matches(INTERACTIVE_SELECTOR)
  );
  const clipped = visible.filter((element) =>
    isViewportClipped(element.getBoundingClientRect(), viewport)
  );
  const undersized = interactive.filter((element) => {
    const rect = element.getBoundingClientRect();
    return rect.width < 24 || rect.height < 24;
  });
  const lowContrast = visible.filter((element) => {
    if (!hasDirectText(element)) return false;
    const style = getComputedStyle(element);
    const foreground = parseCssColor(style.color);
    const background = effectiveBackground(element);
    if (!foreground || !background) return false;
    const size = Number.parseFloat(style.fontSize);
    const threshold =
      size >= 24 || size >= 18.66 && Number(style.fontWeight) >= 700 ? 3 : 4.5;
    return contrastRatio(foreground, background) + 0.01 < threshold;
  });
  const hiddenPrimary = [
    ...root.querySelectorAll<HTMLElement>(PRIMARY_SELECTOR),
  ]
    .filter((element) =>
      !isVisible(element) ||
      isViewportClipped(element.getBoundingClientRect(), viewport)
    );
  const samples: VisualIssueSample[] = [];
  sampleElements(samples, "clipped_content", clipped);
  sampleElements(samples, "undersized_target", undersized);
  sampleElements(samples, "low_contrast", lowContrast);
  sampleElements(samples, "hidden_primary_action", hiddenPrimary);
  if (counters.consoleErrors > 0) {
    samples.push({
      category: "console_error",
      semantic_path: "preview/console",
    });
  }
  if (counters.resourceFailures > 0) {
    samples.push({
      category: "resource_failure",
      semantic_path: "preview/resource",
    });
  }
  if (counters.layoutShiftMilli >= 100) {
    samples.push({
      category: "layout_instability",
      semantic_path: "preview/layout",
    });
  }
  const semanticRows = visible.map((element) => {
    const rect = roundedRect(element.getBoundingClientRect());
    return `${
      semanticPath(element)
    }:${rect.x},${rect.y},${rect.width},${rect.height}`;
  });
  return {
    capture_id: request.capture_id,
    viewport,
    color_scheme: request.color_scheme,
    locale: request.locale,
    scenario: request.scenario,
    reduced_motion: request.reduced_motion,
    document_width: Math.max(
      viewport.width,
      Math.round(document.documentElement.scrollWidth),
      Math.round(root.scrollWidth),
    ),
    document_height: Math.max(
      viewport.height,
      Math.round(document.documentElement.scrollHeight),
      Math.round(root.scrollHeight),
    ),
    element_count: visible.length,
    interactive_count: interactive.length,
    clipped_count: clipped.length,
    undersized_target_count: undersized.length,
    low_contrast_count: lowContrast.length,
    hidden_primary_action_count: hiddenPrimary.length,
    console_error_count: Math.min(counters.consoleErrors, 100_000),
    resource_failure_count: Math.min(counters.resourceFailures, 100_000),
    layout_shift_milli: Math.min(counters.layoutShiftMilli, 10_000),
    semantic_digest: await sha256(semanticRows.join("\n")),
    samples: samples.slice(0, MAX_SAMPLES),
  };
}

async function settleLayout(
  expected: VisualQualityMeasureRequest["viewport"],
): Promise<void> {
  // Chromium can indefinitely suspend requestAnimationFrame for an isolated
  // capture frame that is intentionally transparent and far off-screen. Two
  // task turns let React commit the imported component without depending on a
  // visible frame clock. CI can briefly expose a new iframe as 1x1 before its
  // host dimensions propagate, so do not capture until the child viewport
  // itself matches the fixed Rust-owned matrix.
  const deadline = performance.now() + 2_000;
  while (
    !viewportMatches(globalThis.innerWidth, globalThis.innerHeight, expected)
  ) {
    if (performance.now() >= deadline) {
      throw new Error(
        `Capture viewport ${globalThis.innerWidth}×${globalThis.innerHeight} did not settle at ${expected.width}×${expected.height}.`,
      );
    }
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
  }
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

export function viewportMatches(
  width: number,
  height: number,
  expected: { width: number; height: number },
): boolean {
  return Math.round(width) === expected.width &&
    Math.round(height) === expected.height;
}

function isVisible(element: HTMLElement): boolean {
  const style = getComputedStyle(element);
  const rect = element.getBoundingClientRect();
  return style.display !== "none" && style.visibility !== "hidden" &&
    Number.parseFloat(style.opacity || "1") > 0 && rect.width > 0 &&
    rect.height > 0;
}

function isViewportClipped(
  rect: DOMRect,
  viewport: { width: number; height: number },
): boolean {
  if (
    rect.right <= 0 || rect.bottom <= 0 || rect.left >= viewport.width ||
    rect.top >= viewport.height
  ) return false;
  return rect.left < -1 || rect.top < -1 || rect.right > viewport.width + 1 ||
    rect.bottom > viewport.height + 1;
}

function hasDirectText(element: HTMLElement): boolean {
  return [...element.childNodes].some((node) =>
    node.nodeType === Node.TEXT_NODE && Boolean(node.textContent?.trim())
  );
}

type Rgb = readonly [number, number, number];

export function parseCssColor(value: string): Rgb | undefined {
  const match = value.match(
    /^rgba?\(\s*(\d+(?:\.\d+)?)\s*[, ]\s*(\d+(?:\.\d+)?)\s*[, ]\s*(\d+(?:\.\d+)?)(?:\s*[,/]\s*(\d+(?:\.\d+)?%?))?\s*\)$/,
  );
  if (!match) return undefined;
  const alpha = match[4]?.endsWith("%")
    ? Number.parseFloat(match[4]) / 100
    : Number.parseFloat(match[4] ?? "1");
  if (alpha < 0.98) return undefined;
  return [
    clampByte(Number.parseFloat(match[1])),
    clampByte(Number.parseFloat(match[2])),
    clampByte(Number.parseFloat(match[3])),
  ];
}

function effectiveBackground(element: HTMLElement): Rgb | undefined {
  let current: HTMLElement | null = element;
  while (current) {
    const color = parseCssColor(getComputedStyle(current).backgroundColor);
    if (color) return color;
    current = current.parentElement;
  }
  return parseCssColor(
    getComputedStyle(document.documentElement).backgroundColor,
  ) ??
    [255, 255, 255];
}

export function contrastRatio(left: Rgb, right: Rgb): number {
  const lighter = Math.max(luminance(left), luminance(right));
  const darker = Math.min(luminance(left), luminance(right));
  return (lighter + 0.05) / (darker + 0.05);
}

function luminance(color: Rgb): number {
  const [red, green, blue] = color.map((value) => {
    const channel = value / 255;
    return channel <= 0.03928
      ? channel / 12.92
      : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return red * 0.2126 + green * 0.7152 + blue * 0.0722;
}

function sampleElements(
  output: VisualIssueSample[],
  category: VisualFindingCategory,
  elements: HTMLElement[],
): void {
  for (const element of elements.slice(0, 4)) {
    output.push({
      category,
      semantic_path: semanticPath(element),
      rect: roundedRect(element.getBoundingClientRect()),
    });
  }
}

function semanticPath(element: HTMLElement): string {
  const parts: string[] = [];
  let current: HTMLElement | null = element;
  while (current && current.id !== "root" && parts.length < 6) {
    const siblings = current.parentElement
      ? [...current.parentElement.children].filter((child) =>
        child.tagName === current?.tagName
      )
      : [];
    const index = Math.max(0, siblings.indexOf(current));
    parts.unshift(`${current.tagName.toLowerCase()}[${index}]`);
    current = current.parentElement;
  }
  return parts.length > 0 ? `root/${parts.join("/")}` : "root";
}

function roundedRect(rect: DOMRect): VisualRect {
  return {
    x: Math.round(rect.x),
    y: Math.round(rect.y),
    width: Math.max(0, Math.round(rect.width)),
    height: Math.max(0, Math.round(rect.height)),
  };
}

function clampByte(value: number): number {
  return Math.max(0, Math.min(255, value));
}

async function sha256(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
