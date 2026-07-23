import { join, relative } from "@std/path";
import type {
  WebRendererFile,
  WebRendererManifest,
} from "./package_web_renderers.ts";

const root = Deno.args[0] ?? "dist/web-renderers";
const manifest = JSON.parse(
  await Deno.readTextFile(join(root, "manifest.json")),
) as WebRendererManifest;

if (
  manifest.schema_version !== 1 ||
  manifest.product !== "hyper-term-web-renderers" ||
  manifest.host_contract.native_sdk_web_target !== false ||
  manifest.host_contract.terminal_requires_gateway !== true ||
  manifest.host_contract.authoritative_operations_require_rust !== true
) {
  throw new Error("web renderer manifest has an invalid host contract");
}

const actual = await inventory(root);
const expected = [...manifest.files].sort((left, right) =>
  left.path.localeCompare(right.path)
);
if (JSON.stringify(actual) !== JSON.stringify(expected)) {
  throw new Error("web renderer package does not match its SHA-256 inventory");
}

console.log(
  `Hyper Term Web Renderer Kit verified: ${actual.length} files, ` +
    `Workbench WebAssembly and Rust host boundary intact`,
);

async function inventory(directory: string): Promise<WebRendererFile[]> {
  const files: WebRendererFile[] = [];
  for await (const path of walkFiles(directory)) {
    if (relative(directory, path) === "manifest.json") continue;
    const bytes = await Deno.readFile(path);
    files.push({
      path: relative(directory, path),
      bytes: bytes.byteLength,
      sha256: toHex(await crypto.subtle.digest("SHA-256", bytes)),
    });
  }
  return files.sort((left, right) => left.path.localeCompare(right.path));
}

async function* walkFiles(directory: string): AsyncGenerator<string> {
  for await (const entry of Deno.readDir(directory)) {
    const path = join(directory, entry.name);
    const info = await Deno.lstat(path);
    if (info.isSymlink) {
      throw new Error(`web renderer package rejects symbolic links: ${path}`);
    }
    if (info.isDirectory) yield* walkFiles(path);
    else if (info.isFile) yield path;
    else throw new Error(`web renderer package rejects special files: ${path}`);
  }
}

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
