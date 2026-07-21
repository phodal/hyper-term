import type * as Esbuild from "esbuild-wasm";
import type { ArtifactCandidate, CompileDiagnostic } from "../protocol.ts";
import {
  type CompileRequest,
  type CompileResponse,
  validateCompileRequest,
} from "./compiler-protocol.ts";

const CAPSULES = new Set([
  "react",
  "react/jsx-runtime",
  "react/jsx-dev-runtime",
  "@hyper/runtime",
]);
const STATIC_REQUIRE = /\brequire\((['"])([^'"\n]+)\1\)/g;
const UNSUPPORTED_MODULE_SYNTAX = /\b(?:import\s*\(|import\.meta|require\s*\()/;
const UNSUPPORTED_CSS_SYNTAX = /@import\b|url\s*\(/i;

interface CachedModule {
  source: string;
  code: string;
  map: Record<string, unknown>;
  dependencies: string[];
  warnings: CompileDiagnostic[];
}

interface SliceState {
  generation: number;
  modules: Map<string, CachedModule>;
}

interface IndexedSourceMapSection {
  offset: { line: number; column: number };
  map: Record<string, unknown>;
}

export interface PreviewTransformBackend {
  version: string;
  transform(
    input: string,
    options: Esbuild.TransformOptions,
  ): Promise<Esbuild.TransformResult>;
}

const state: SliceState = {
  generation: 0,
  modules: new Map(),
};

export function canUsePreviewSliceCompiler(request: CompileRequest): boolean {
  return Object.entries(request.files).every(([path, source]) => {
    if (path.endsWith(".css")) return !UNSUPPORTED_CSS_SYNTAX.test(source);
    if (path.endsWith(".json")) return true;
    return !UNSUPPORTED_MODULE_SYNTAX.test(source);
  });
}

export async function compilePreviewSlice(
  request: CompileRequest,
  backend: PreviewTransformBackend,
): Promise<CompileResponse> {
  validateCompileRequest(request);
  const generation = ++state.generation;
  const nextModules = new Map<string, CachedModule>();
  const transformPaths = Object.keys(request.files)
    .filter((path) => !path.endsWith(".css"))
    .sort();
  const pending: string[] = [];
  for (const path of transformPaths) {
    const source = request.files[path];
    const cached = state.modules.get(path);
    if (cached?.source === source) nextModules.set(path, cached);
    else pending.push(path);
  }

  for (let offset = 0; offset < pending.length; offset += 16) {
    assertCurrent(generation);
    const paths = pending.slice(offset, offset + 16);
    const transformed = await Promise.all(paths.map(async (path) => {
      const source = request.files[path];
      const result = await backend.transform(source, {
        sourcefile: `hyper-vfs:${path}`,
        loader: loaderFor(path),
        format: "cjs",
        platform: "browser",
        target: "es2022",
        jsx: "automatic",
        jsxImportSource: "react",
        sourcemap: "external",
        sourcesContent: false,
        logLevel: "silent",
      });
      return [path, {
        source,
        code: result.code,
        map: parseSourceMap(result.map, path),
        dependencies: dependenciesFrom(result.code),
        warnings: result.warnings.map((warning) =>
          diagnostic(warning, "warning")
        ),
      }] as const;
    }));
    assertCurrent(generation);
    for (const [path, compiled] of transformed) {
      nextModules.set(path, compiled);
    }
  }

  const graph = reachableGraph(request, nextModules);
  const output = composeBundle(request.entrypoint, graph.modules);
  const css = graph.cssPaths.map((path) => request.files[path]).join("\n");
  const contentDigest = await sha256(output.bundle + css);
  assertCurrent(generation);
  state.modules = nextModules;

  const candidate: ArtifactCandidate = {
    schema_version: 1,
    source_revision: request.source_revision,
    entrypoint: request.entrypoint,
    bundle: output.bundle,
    css,
    source_map: JSON.stringify({
      version: 3,
      sections: output.sections,
    }),
    content_digest: contentDigest,
    compiler: { name: "esbuild-wasm", version: backend.version },
    diagnostics: graph.modules.flatMap((module) => module.compiled.warnings),
  };
  return {
    type: "compiled",
    request_id: request.request_id,
    source_revision: request.source_revision,
    candidate,
  };
}

export function cancelPreviewSliceCompiler(): void {
  state.generation += 1;
}

export function resetPreviewSliceCompilerForTest(): void {
  state.generation += 1;
  state.modules.clear();
}

function reachableGraph(
  request: CompileRequest,
  modules: Map<string, CachedModule>,
): {
  modules: Array<{ path: string; compiled: CachedModule }>;
  cssPaths: string[];
} {
  const orderedModules: Array<{ path: string; compiled: CachedModule }> = [];
  const cssPaths: string[] = [];
  const visited = new Set<string>();
  const visit = (path: string): void => {
    if (visited.has(path)) return;
    visited.add(path);
    if (path.endsWith(".css")) {
      if (!(path in request.files)) throw undeclared(path);
      cssPaths.push(path);
      return;
    }
    const compiled = modules.get(path);
    if (!compiled) throw undeclared(path);
    orderedModules.push({ path, compiled });
    for (const specifier of compiled.dependencies) {
      if (CAPSULES.has(specifier)) continue;
      visit(resolveVirtualPath(specifier, path));
    }
  };
  visit(request.entrypoint);
  return { modules: orderedModules, cssPaths };
}

function composeBundle(
  entrypoint: string,
  modules: Array<{ path: string; compiled: CachedModule }>,
): { bundle: string; sections: IndexedSourceMapSection[] } {
  const chunks: string[] = [];
  const sections: IndexedSourceMapSection[] = [];
  let line = 0;
  const append = (value: string): void => {
    chunks.push(value);
    line += value.split("\n").length - 1;
  };
  append(`(() => {\n${runtimePrelude()}\n`);
  for (const { path, compiled } of modules) {
    append(
      `__modules[${JSON.stringify(path)}] = (module, exports, require) => {\n`,
    );
    sections.push({ offset: { line, column: 0 }, map: compiled.map });
    append(compiled.code.endsWith("\n") ? compiled.code : `${compiled.code}\n`);
    append("};\n");
  }
  append(`const __entry = __load(${JSON.stringify(entrypoint)}, "/");\n`);
  append("globalThis.__HYPER_MOUNT__(__entry.default);\n})();\n");
  return { bundle: chunks.join(""), sections };
}

function runtimePrelude(): string {
  return `const __modules = Object.create(null);
const __cache = Object.create(null);
const __capsules = {
  "react": () => globalThis.__HYPER_REACT__,
  "react/jsx-runtime": () => globalThis.__HYPER_JSX_RUNTIME__,
  "react/jsx-dev-runtime": () => globalThis.__HYPER_JSX_DEV_RUNTIME__,
  "@hyper/runtime": () => ({
    mount: globalThis.__HYPER_MOUNT__,
    traceAction: (name, payload = null) => globalThis.__HYPER_TRACE__("action", name, payload),
    traceCheckpoint: (name, payload = null) => globalThis.__HYPER_TRACE__("checkpoint", name, payload),
    useReplayReducer: (name, reducer, initialState) => globalThis.__HYPER_USE_REPLAY_REDUCER__(name, reducer, initialState),
    replayableEffect: (name, input, invoke) => globalThis.__HYPER_EFFECT__(name, input, invoke),
  }),
};
const __parent = (path) => path.slice(0, path.lastIndexOf("/")) || "/";
const __resolve = (specifier, importer) => {
  if (__capsules[specifier]) return specifier;
  if (specifier.startsWith("/")) return specifier;
  if (!specifier.startsWith(".")) return specifier;
  const parts = (__parent(importer) + "/" + specifier).split("/");
  const normalized = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") normalized.pop();
    else normalized.push(part);
  }
  return "/" + normalized.join("/");
};
const __load = (specifier, importer) => {
  const path = __resolve(specifier, importer);
  if (__capsules[path]) return __capsules[path]();
  if (__cache[path]) return __cache[path].exports;
  const factory = __modules[path];
  if (!factory) {
    if (path.endsWith(".css")) return {};
    throw new Error("undeclared virtual import: " + specifier);
  }
  const module = { exports: {} };
  __cache[path] = module;
  factory(module, module.exports, (next) => __load(next, path));
  return module.exports;
};`;
}

function dependenciesFrom(code: string): string[] {
  const dependencies: string[] = [];
  for (const match of code.matchAll(STATIC_REQUIRE)) {
    dependencies.push(match[2]);
  }
  return [...new Set(dependencies)];
}

function resolveVirtualPath(specifier: string, importer: string): string {
  if (specifier.startsWith("/")) return normalize(specifier);
  if (!specifier.startsWith(".")) return specifier;
  return normalize(`${parentPath(importer)}/${specifier}`);
}

function normalize(path: string): string {
  const parts: string[] = [];
  for (const part of path.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  return `/${parts.join("/")}`;
}

function parentPath(path: string): string {
  const separator = path.lastIndexOf("/");
  return separator <= 0 ? "/" : path.slice(0, separator);
}

function loaderFor(path: string): Esbuild.Loader {
  if (path.endsWith(".tsx")) return "tsx";
  if (path.endsWith(".ts")) return "ts";
  if (path.endsWith(".jsx")) return "jsx";
  if (path.endsWith(".json")) return "json";
  return "js";
}

function parseSourceMap(value: string, path: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(value) as Record<string, unknown>;
    if (parsed.version !== 3 || !Array.isArray(parsed.sources)) {
      throw new Error();
    }
    return parsed;
  } catch {
    throw new Error(`compiler emitted an invalid source map for ${path}`);
  }
}

function diagnostic(
  message: Esbuild.Message,
  severity: "error" | "warning",
): CompileDiagnostic {
  return {
    severity,
    text: message.text,
    file: message.location?.file,
    line: message.location?.line,
    column: message.location?.column,
  };
}

function undeclared(path: string): Error {
  return new Error(`undeclared virtual import: ${path}`);
}

function assertCurrent(generation: number): void {
  if (generation !== state.generation) {
    throw new Error("preview slice compilation was superseded");
  }
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
