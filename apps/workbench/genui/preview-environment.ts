export interface PreviewQualityEnvironment {
  colorScheme: "light" | "dark";
  reducedMotion: boolean;
}

const TRUE_MEDIA_QUERY = "(min-width: 0px)";
const FALSE_MEDIA_QUERY = "(max-width: -1px)";

export function previewQualityEnvironment(
  url: URL,
): PreviewQualityEnvironment | undefined {
  const colorScheme = url.searchParams.get("quality_color_scheme");
  const reducedMotion = url.searchParams.get("quality_reduced_motion");
  if (colorScheme === null && reducedMotion === null) return undefined;
  if (
    colorScheme !== "light" && colorScheme !== "dark" ||
    reducedMotion !== "reduce" && reducedMotion !== "no-preference"
  ) return undefined;
  return {
    colorScheme,
    reducedMotion: reducedMotion === "reduce",
  };
}

export function applyPreviewQualityEnvironment(
  environment: PreviewQualityEnvironment,
): void {
  document.documentElement.dataset.hyperColorScheme = environment.colorScheme;
  document.documentElement.dataset.hyperReducedMotion =
    environment.reducedMotion ? "reduce" : "no-preference";
  document.documentElement.style.colorScheme = environment.colorScheme;
  document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]')
    ?.setAttribute(
      "content",
      environment.colorScheme,
    );

  const nativeMatchMedia = globalThis.matchMedia?.bind(globalThis);
  if (!nativeMatchMedia) return;
  globalThis.matchMedia = (query: string): MediaQueryList => {
    const rewritten = rewritePreferenceMediaQuery(query, environment);
    const nativeResult = nativeMatchMedia(rewritten);
    return new Proxy(nativeResult, {
      get(target, property) {
        if (property === "media") return query;
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
  };
}

export function rewritePreferenceMediaStyle(
  style: HTMLStyleElement,
  environment: PreviewQualityEnvironment,
): void {
  const rules = style.sheet?.cssRules;
  if (!rules) return;
  style.textContent = [...rules]
    .map((rule) => rewriteRule(rule, environment))
    .join("\n");
}

export function rewritePreferenceMediaQuery(
  query: string,
  environment: PreviewQualityEnvironment,
): string {
  return query
    .replace(
      /\(\s*prefers-color-scheme\s*:\s*(light|dark)\s*\)/gi,
      (_match, requested: string) =>
        requested.toLowerCase() === environment.colorScheme
          ? TRUE_MEDIA_QUERY
          : FALSE_MEDIA_QUERY,
    )
    .replace(
      /\(\s*prefers-reduced-motion\s*:\s*(reduce|no-preference)\s*\)/gi,
      (_match, requested: string) =>
        (requested.toLowerCase() === "reduce") === environment.reducedMotion
          ? TRUE_MEDIA_QUERY
          : FALSE_MEDIA_QUERY,
    );
}

function rewriteRule(
  rule: CSSRule,
  environment: PreviewQualityEnvironment,
): string {
  if (!(rule instanceof CSSMediaRule)) return rule.cssText;
  const condition = rewritePreferenceMediaQuery(
    rule.conditionText,
    environment,
  );
  const children = [...rule.cssRules]
    .map((child) => rewriteRule(child, environment))
    .join("\n");
  return `@media ${condition} {\n${children}\n}`;
}
