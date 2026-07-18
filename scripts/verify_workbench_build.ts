import { basename, relative } from "@std/path";

const root = new URL("../dist/workbench/", import.meta.url);
const forbidden = [
  "vite/client",
  "@vitejs/",
  "__vite__",
  "localhost:",
  "127.0.0.1:",
  "ws://",
];
const files: Array<{ path: string; bytes: number; sha256: string }> = [];
let previewDocument = "";

for await (const entry of walk(root)) {
  if (!entry.isFile || basename(entry.path) === "build-manifest.json") {
    continue;
  }
  const bytes = await Deno.readFile(entry.path);
  const text = isText(entry.path) ? new TextDecoder().decode(bytes) : "";
  if (relative(root.pathname, entry.path) === "genui/preview.html") {
    previewDocument = text;
  }
  for (const token of forbidden) {
    if (text.includes(token)) {
      throw new Error(
        `${relative(root.pathname, entry.path)} contains ${token}`,
      );
    }
  }
  files.push({
    path: relative(root.pathname, entry.path),
    bytes: bytes.byteLength,
    sha256: toHex(await crypto.subtle.digest("SHA-256", bytes)),
  });
}

if (
  !previewDocument.includes("hyper_term_preview_boot") ||
  /<script\s+[^>]*src=/i.test(previewDocument)
) {
  throw new Error("isolated preview must contain one inline runtime capsule");
}

files.sort((left, right) => left.path.localeCompare(right.path));
for (
  const required of [
    "index.html",
    "compiler.worker.js",
    "esbuild.wasm",
    "genui/preview.html",
  ]
) {
  if (!files.some((file) => file.path === required)) {
    throw new Error(`workbench build is missing ${required}`);
  }
}

const manifest = {
  schema_version: 1,
  builder: { runtime: "deno", version: Deno.version.deno },
  files,
};
await Deno.writeTextFile(
  new URL("build-manifest.json", root),
  `${JSON.stringify(manifest, null, 2)}\n`,
);

async function* walk(
  rootUrl: URL,
): AsyncGenerator<Deno.DirEntry & { path: string }> {
  for await (const entry of Deno.readDir(rootUrl)) {
    const url = new URL(entry.name, rootUrl);
    const path = decodeURIComponent(url.pathname);
    if (entry.isDirectory) {
      yield* walk(new URL(`${entry.name}/`, rootUrl));
    } else {
      yield { ...entry, path };
    }
  }
}

function isText(path: string): boolean {
  return /\.(?:css|html|js|json|map)$/.test(path);
}

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
