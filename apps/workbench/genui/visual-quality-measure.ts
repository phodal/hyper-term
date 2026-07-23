export type VisualFindingCategory =
  | "empty_render"
  | "viewport_overflow"
  | "clipped_content"
  | "undersized_target"
  | "low_contrast"
  | "hidden_primary_action"
  | "missing_focus_indicator"
  | "console_error"
  | "resource_failure"
  | "layout_instability";

export interface VisualQualityMeasureRequest {
  capture_id: string;
  viewport: { width: number; height: number };
  color_scheme: "light" | "dark";
  locale: "en" | "zh-CN";
  scenario: VisualQualityScenario;
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
  locale: "en" | "zh-CN";
  scenario: VisualQualityScenario;
  reduced_motion: boolean;
  document_width: number;
  document_height: number;
  element_count: number;
  interactive_count: number;
  clipped_count: number;
  undersized_target_count: number;
  low_contrast_count: number;
  hidden_primary_action_count: number;
  focus_target_count: number;
  focus_visible_count: number;
  content_fixture_digest?: string;
  content_fixture_target_count: number;
  content_fixture_applied_count: number;
  content_fixture_cjk_label_count: number;
  content_fixture_long_content_count: number;
  declared_state_digest?: string;
  declared_state_target_count: number;
  declared_state_applied_count: number;
  declared_state_semantic_count: number;
  console_error_count: number;
  resource_failure_count: number;
  layout_shift_milli: number;
  semantic_digest: string;
  samples: VisualIssueSample[];
}

export type VisualQualityScenario =
  | "default"
  | "focus-first"
  | "content-stress"
  | "state-empty"
  | "state-loading"
  | "state-error"
  | "state-disabled";

const INTERACTIVE_SELECTOR =
  "button,a[href],input,select,textarea,[role=button],[tabindex]";
const PRIMARY_SELECTOR = '[data-primary-action="true"]';
const MAX_SAMPLES = 24;
export const CONTENT_STRESS_FIXTURE_ID = "hyper-term-cjk-long-content-v1";
const CONTENT_STRESS_CJK_LABEL = "运行完整的生成式界面验证流程并查看所有更改";
const CONTENT_STRESS_LONG_BODY =
  "这是由宿主注入的中文长内容验证文本，用于检查人工智能生成界面在窄窗口中的换行、行高、可读性和布局稳定性。内容需要自然折行，不能遮挡主要操作，也不能让页面产生水平滚动。终端用户可能同时查看命令结果、代码差异、审批信息和代理执行状态，因此界面必须在有限空间内保持清晰的阅读顺序。";

export async function measureVisualQuality(
  root: HTMLElement,
  request: VisualQualityMeasureRequest,
  counters: VisualQualityRuntimeCounters,
): Promise<VisualCaptureObservation> {
  document.documentElement.lang = request.locale;
  const contentFixture = applyContentStressFixture(root, request.scenario);
  const declaredState = applyDeclaredStateScenario(root, request.scenario);
  await settleLayout(request.viewport);
  const contentCoverage = inspectContentStressFixture(contentFixture);
  const stateCoverage = await inspectDeclaredStateScenario(declaredState);
  const focus = await measureFocusScenario(root, request.scenario);
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
      !belongsToInactiveDeclaredState(element) &&
      (!isVisible(element) ||
        isViewportClipped(element.getBoundingClientRect(), viewport))
    );
  const samples: VisualIssueSample[] = [];
  sampleElements(samples, "clipped_content", clipped);
  sampleElements(samples, "undersized_target", undersized);
  sampleElements(samples, "low_contrast", lowContrast);
  sampleElements(samples, "hidden_primary_action", hiddenPrimary);
  if (focus.target && !focus.visible) {
    samples.push({
      category: "missing_focus_indicator",
      semantic_path: semanticPath(focus.target),
      rect: roundedRect(focus.target.getBoundingClientRect()),
    });
  }
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
    focus_target_count: focus.target ? 1 : 0,
    focus_visible_count: focus.visible ? 1 : 0,
    ...(request.scenario === "content-stress"
      ? { content_fixture_digest: await sha256(CONTENT_STRESS_FIXTURE_ID) }
      : {}),
    content_fixture_target_count: contentCoverage.targetCount,
    content_fixture_applied_count: contentCoverage.appliedCount,
    content_fixture_cjk_label_count: contentCoverage.cjkLabelCount,
    content_fixture_long_content_count: contentCoverage.longContentCount,
    ...(stateCoverage.digest
      ? { declared_state_digest: stateCoverage.digest }
      : {}),
    declared_state_target_count: stateCoverage.targetCount,
    declared_state_applied_count: stateCoverage.appliedCount,
    declared_state_semantic_count: stateCoverage.semanticCount,
    console_error_count: Math.min(counters.consoleErrors, 100_000),
    resource_failure_count: Math.min(counters.resourceFailures, 100_000),
    layout_shift_milli: Math.min(counters.layoutShiftMilli, 10_000),
    semantic_digest: await sha256(semanticRows.join("\n")),
    samples: samples.slice(0, MAX_SAMPLES),
  };
}

interface ContentStressFixture {
  cjkLabelTarget?: HTMLElement;
  longContentTarget?: HTMLElement;
}

function applyContentStressFixture(
  root: HTMLElement,
  scenario: VisualQualityMeasureRequest["scenario"],
): ContentStressFixture {
  if (scenario !== "content-stress") return {};
  const visible = [root, ...root.querySelectorAll<HTMLElement>("*")]
    .filter((element): element is HTMLElement =>
      element instanceof HTMLElement && isVisible(element)
    );
  const cjkLabelTarget = visible.find((element) =>
    element.matches(INTERACTIVE_SELECTOR) && hasDirectText(element)
  );
  const longContentTarget = visible.find((element) =>
    element !== cjkLabelTarget && !element.matches(INTERACTIVE_SELECTOR) &&
    hasDirectText(element)
  );
  if (cjkLabelTarget) {
    replaceDirectText(cjkLabelTarget, CONTENT_STRESS_CJK_LABEL);
  }
  if (longContentTarget) {
    replaceDirectText(longContentTarget, CONTENT_STRESS_LONG_BODY);
  }
  return { cjkLabelTarget, longContentTarget };
}

function inspectContentStressFixture(fixture: ContentStressFixture): {
  targetCount: number;
  appliedCount: number;
  cjkLabelCount: number;
  longContentCount: number;
} {
  const targets = [fixture.cjkLabelTarget, fixture.longContentTarget]
    .filter((target): target is HTMLElement => Boolean(target));
  const cjkLabelCount = binaryEvidence(Boolean(
    fixture.cjkLabelTarget?.isConnected &&
      fixture.cjkLabelTarget.textContent?.includes(CONTENT_STRESS_CJK_LABEL),
  ));
  const longContentCount = binaryEvidence(Boolean(
    fixture.longContentTarget?.isConnected &&
      fixture.longContentTarget.textContent?.includes(
        CONTENT_STRESS_LONG_BODY,
      ),
  ));
  return {
    targetCount: targets.length,
    appliedCount: cjkLabelCount + longContentCount,
    cjkLabelCount,
    longContentCount,
  };
}

export function binaryEvidence(value: unknown): 0 | 1 {
  return value === true ? 1 : 0;
}

function replaceDirectText(element: HTMLElement, value: string): void {
  const textNode = [...element.childNodes].find((node) =>
    node.nodeType === Node.TEXT_NODE && Boolean(node.textContent?.trim())
  );
  if (textNode) textNode.textContent = value;
}

type DeclaredStateName = "empty" | "loading" | "error" | "disabled";

interface DeclaredStateFixture {
  state?: DeclaredStateName;
  targets: HTMLElement[];
}

function applyDeclaredStateScenario(
  root: HTMLElement,
  scenario: VisualQualityScenario,
): DeclaredStateFixture {
  const state = declaredStateName(scenario);
  if (!state) return { targets: [] };
  const declared = [
    root,
    ...root.querySelectorAll<HTMLElement>(
      "[data-hyper-state]",
    ),
  ].filter((element): element is HTMLElement =>
    element instanceof HTMLElement && element.hasAttribute("data-hyper-state")
  );
  const targets = declared.filter((element) =>
    element.dataset.hyperState === state
  );
  if (targets.length > 32) {
    throw new Error(`Declared state ${state} exceeds the 32 target bound.`);
  }
  for (const element of declared) {
    const active = targets.includes(element);
    element.hidden = !active;
    if (active) element.dataset.hyperStateActive = "true";
    else delete element.dataset.hyperStateActive;
  }
  return { state, targets };
}

async function inspectDeclaredStateScenario(
  fixture: DeclaredStateFixture,
): Promise<{
  digest?: string;
  targetCount: number;
  appliedCount: number;
  semanticCount: number;
}> {
  if (!fixture.state) {
    return { targetCount: 0, appliedCount: 0, semanticCount: 0 };
  }
  const applied = fixture.targets.filter((target) =>
    target.isConnected && target.dataset.hyperStateActive === "true" &&
    isVisible(target)
  );
  const semantic = applied.filter((target) =>
    declaredStateSemanticsPresent(fixture.state!, target)
  );
  const digestRows = fixture.targets.map((target) =>
    [
      fixture.state,
      semanticPath(target),
      target.textContent?.trim().length ?? 0,
      target.getAttribute("role") ?? "",
      target.getAttribute("aria-busy") ?? "",
      target.dataset.hyperStateFeedback === "true" ? 1 : 0,
    ].join(":")
  );
  return {
    digest: await sha256([
      fixture.state,
      fixture.targets.length,
      applied.length,
      semantic.length,
      ...digestRows,
    ].join("\n")),
    targetCount: fixture.targets.length,
    appliedCount: applied.length,
    semanticCount: semantic.length,
  };
}

function declaredStateSemanticsPresent(
  state: DeclaredStateName,
  target: HTMLElement,
): boolean {
  const descendants = [target, ...target.querySelectorAll<HTMLElement>("*")]
    .filter((element): element is HTMLElement =>
      element instanceof HTMLElement && isVisible(element)
    );
  const feedback = descendants.some((element) =>
    element.dataset.hyperStateFeedback === "true" &&
    Boolean(element.textContent?.trim())
  );
  const busy = descendants.some((element) =>
    element.getAttribute("aria-busy") === "true" ||
    element.getAttribute("role") === "status"
  );
  const alert = descendants.some((element) =>
    element.getAttribute("role") === "alert" ||
    element.getAttribute("aria-live") === "assertive"
  );
  const disabled = descendants.some((element) =>
    element.getAttribute("aria-disabled") === "true" ||
    "disabled" in element && Boolean(element.disabled)
  );
  return declaredStateSemanticEvidence(state, {
    feedback,
    busy,
    alert,
    disabled,
  }) === 1;
}

export function declaredStateSemanticEvidence(
  state: DeclaredStateName,
  evidence: {
    feedback: boolean;
    busy: boolean;
    alert: boolean;
    disabled: boolean;
  },
): 0 | 1 {
  if (!evidence.feedback) return 0;
  if (state === "loading") return binaryEvidence(evidence.busy);
  if (state === "error") return binaryEvidence(evidence.alert);
  if (state === "disabled") return binaryEvidence(evidence.disabled);
  return 1;
}

function declaredStateName(
  scenario: VisualQualityScenario,
): DeclaredStateName | undefined {
  if (!scenario.startsWith("state-")) return undefined;
  const state = scenario.slice("state-".length);
  if (
    state === "empty" || state === "loading" || state === "error" ||
    state === "disabled"
  ) return state;
  return undefined;
}

function belongsToInactiveDeclaredState(element: HTMLElement): boolean {
  const owner = element.closest<HTMLElement>("[data-hyper-state]");
  return Boolean(owner?.hidden);
}

interface FocusIndicatorStyle {
  outlineStyle: string;
  outlineWidth: string;
  outlineColor: string;
  outlineOffset: string;
  boxShadow: string;
  borderColor: string;
  backgroundColor: string;
  color: string;
}

async function measureFocusScenario(
  root: HTMLElement,
  scenario: VisualQualityMeasureRequest["scenario"],
): Promise<{ target?: HTMLElement; visible: boolean }> {
  if (scenario !== "focus-first") return { visible: false };
  const target = [
    root,
    ...root.querySelectorAll<HTMLElement>(INTERACTIVE_SELECTOR),
  ].find((element): element is HTMLElement =>
    element instanceof HTMLElement && isKeyboardFocusable(element) &&
    isVisible(element)
  );
  if (!target) return { visible: false };
  const before = focusIndicatorStyle(getComputedStyle(target));
  target.focus(
    {
      preventScroll: true,
      focusVisible: true,
    } as FocusOptions & { focusVisible: boolean },
  );
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  const after = focusIndicatorStyle(getComputedStyle(target));
  const visible = document.activeElement === target &&
    target.matches(":focus-visible") && focusIndicatorChanged(before, after);
  return { target, visible };
}

function isKeyboardFocusable(element: HTMLElement): boolean {
  if (element.tabIndex < 0 || element.hasAttribute("inert")) return false;
  if (element.getAttribute("aria-disabled") === "true") return false;
  return !("disabled" in element && Boolean(element.disabled));
}

function focusIndicatorStyle(style: CSSStyleDeclaration): FocusIndicatorStyle {
  return {
    outlineStyle: style.outlineStyle,
    outlineWidth: style.outlineWidth,
    outlineColor: style.outlineColor,
    outlineOffset: style.outlineOffset,
    boxShadow: style.boxShadow,
    borderColor: style.borderColor,
    backgroundColor: style.backgroundColor,
    color: style.color,
  };
}

export function focusIndicatorChanged(
  before: FocusIndicatorStyle,
  after: FocusIndicatorStyle,
): boolean {
  return Object.keys(before).some((key) =>
    before[key as keyof FocusIndicatorStyle] !==
      after[key as keyof FocusIndicatorStyle]
  );
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
