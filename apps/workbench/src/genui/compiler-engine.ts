import type * as Esbuild from "esbuild-wasm";
import type { CompileDiagnostic } from "../protocol.ts";
import {
  type CompileRequest,
  type CompileResponse,
  validateCompileRequest,
} from "./compiler-protocol.ts";

const CAPSULE_SOURCES = new Map([
  [
    "react",
    `const React=globalThis.__HYPER_REACT__; export default React;\n` +
    `export const useState=React.useState,useEffect=React.useEffect,useMemo=React.useMemo,useRef=React.useRef,useCallback=React.useCallback,createElement=React.createElement,Fragment=React.Fragment;`,
  ],
  [
    "react/jsx-runtime",
    `const runtime=globalThis.__HYPER_JSX_RUNTIME__; export const jsx=runtime.jsx,jsxs=runtime.jsxs,Fragment=runtime.Fragment;`,
  ],
  [
    "react/jsx-dev-runtime",
    `const runtime=globalThis.__HYPER_JSX_DEV_RUNTIME__; export const jsxDEV=runtime.jsxDEV,Fragment=runtime.Fragment;`,
  ],
  [
    "@hyper/runtime",
    `export const mount=globalThis.__HYPER_MOUNT__;`,
  ],
]);

let initializePromise: Promise<void> | undefined;
let compiler: typeof Esbuild | undefined;

export async function initializeCompiler(
  backend: typeof Esbuild,
  options: Esbuild.InitializeOptions,
): Promise<void> {
  if (compiler !== undefined && compiler !== backend) {
    throw new Error("compiler backend cannot change inside one runtime");
  }
  compiler = backend;
  initializePromise ??= backend.initialize(options);
  await initializePromise;
}

export async function compileRequest(
  request: CompileRequest,
): Promise<CompileResponse> {
  validateCompileRequest(request);
  const esbuild = compiler;
  if (esbuild === undefined) throw new Error("compiler is not initialized");
  const entry = "/__hyper_entry__.tsx";
  const files = new Map(Object.entries(request.files));
  files.set(
    entry,
    `import Component from ${JSON.stringify(request.entrypoint)};\n` +
      `import { mount } from "@hyper/runtime";\nmount(Component);\n`,
  );
  const result = await esbuild.build({
    absWorkingDir: "/",
    entryPoints: [entry],
    outfile: "artifact.js",
    bundle: true,
    format: "iife",
    platform: "browser",
    target: ["es2022"],
    jsx: "automatic",
    jsxImportSource: "react",
    sourcemap: "external",
    sourcesContent: true,
    write: false,
    logLevel: "silent",
    plugins: [{
      name: "bounded-virtual-filesystem",
      setup(build) {
        build.onResolve({ filter: /.*/ }, (args) => {
          if (CAPSULE_SOURCES.has(args.path)) {
            return { path: args.path, namespace: "hyper-capsule" };
          }
          const path = resolveVirtualPath(args.path, args.importer);
          if (!files.has(path)) {
            return {
              errors: [{ text: `undeclared virtual import: ${args.path}` }],
            };
          }
          return { path, namespace: "hyper-vfs" };
        });
        build.onLoad({ filter: /.*/, namespace: "hyper-vfs" }, (args) => ({
          contents: files.get(args.path),
          loader: loaderFor(args.path),
          resolveDir: parentPath(args.path),
        }));
        build.onLoad({ filter: /.*/, namespace: "hyper-capsule" }, (args) => ({
          contents: CAPSULE_SOURCES.get(args.path),
          loader: "js",
        }));
      },
    }],
  });
  const output = result.outputFiles ?? [];
  const bundle = decode(requiredOutput(output, "artifact.js"));
  const sourceMap = decode(requiredOutput(output, "artifact.js.map"));
  const cssFile = output.find((file) => file.path.endsWith("artifact.css"));
  const css = cssFile ? decode(cssFile) : "";
  const contentDigest = await sha256(bundle + css);

  return {
    type: "compiled",
    request_id: request.request_id,
    source_revision: request.source_revision,
    candidate: {
      schema_version: 1,
      source_revision: request.source_revision,
      entrypoint: request.entrypoint,
      bundle,
      css,
      source_map: sourceMap,
      content_digest: contentDigest,
      compiler: { name: "esbuild-wasm", version: esbuild.version },
      diagnostics: result.warnings.map((warning) =>
        diagnostic(warning, "warning")
      ),
    },
  };
}

export function diagnosticsFrom(error: unknown): CompileDiagnostic[] {
  if (error && typeof error === "object" && "errors" in error) {
    const messages = (error as { errors?: Esbuild.Message[] }).errors ?? [];
    if (messages.length > 0) {
      return messages.map((message) => diagnostic(message, "error"));
    }
  }
  return [{
    severity: "error",
    text: error instanceof Error ? error.message : String(error),
  }];
}

function resolveVirtualPath(path: string, importer: string): string {
  if (path.startsWith("/")) return normalize(path);
  if (!path.startsWith(".")) return path;
  return normalize(`${parentPath(importer || "/")}/${path}`);
}

function normalize(path: string): string {
  const parts: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
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
  if (path.endsWith(".css")) return "css";
  if (path.endsWith(".json")) return "json";
  return "js";
}

function requiredOutput(
  files: Esbuild.OutputFile[],
  suffix: string,
): Esbuild.OutputFile {
  const file = files.find((candidate) => candidate.path.endsWith(suffix));
  if (!file) throw new Error(`compiler did not emit ${suffix}`);
  return file;
}

function decode(file: Esbuild.OutputFile): string {
  return new TextDecoder().decode(file.contents);
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

async function sha256(value: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(value),
  );
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
