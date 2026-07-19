import {
  dirname,
  fromFileUrl,
  isAbsolute,
  join,
  resolve,
  SEPARATOR,
} from "@std/path";

const deno = Deno.execPath();
const root = Deno.args[0]
  ? resolve(Deno.args[0])
  : fromFileUrl(new URL("../dist/runtime/acp/", import.meta.url));
const manifest = JSON.parse(
  await Deno.readTextFile(join(root, "manifest.json")),
) as {
  schema_version: number;
  runtime: { name: string; version: string };
  adapters: Array<{
    provider_id: string;
    version: string;
    entrypoint: string;
    entrypoint_sha256: string;
  }>;
  files: Array<{ path: string; bytes: number; sha256: string }>;
};

if (
  manifest.schema_version !== 1 || manifest.runtime.name !== "deno" ||
  manifest.runtime.version !== Deno.version.deno
) {
  throw new Error("ACP runtime manifest does not match the build runtime");
}
if (manifest.adapters.length !== 2 || manifest.files.length === 0) {
  throw new Error("ACP runtime manifest is incomplete");
}

const canonicalRoot = await Deno.realPath(root);
const fileDigests = new Map<string, string>();
for (const file of manifest.files) {
  if (
    !validManifestPath(file.path) || !Number.isSafeInteger(file.bytes) ||
    file.bytes < 0 || !/^[0-9a-f]{64}$/.test(file.sha256)
  ) {
    throw new Error(`ACP runtime file metadata is invalid: ${file.path}`);
  }
  if (fileDigests.has(file.path)) {
    throw new Error(`ACP runtime file is duplicated: ${file.path}`);
  }
  const path = join(canonicalRoot, ...file.path.split("/"));
  const canonicalPath = await Deno.realPath(path).catch((error) => {
    throw new Error(`ACP runtime file is missing: ${file.path}`, {
      cause: error,
    });
  });
  if (
    canonicalPath !== canonicalRoot &&
    !canonicalPath.startsWith(`${canonicalRoot}${SEPARATOR}`)
  ) {
    throw new Error(`ACP runtime file escapes its root: ${file.path}`);
  }
  const metadata = await Deno.stat(canonicalPath);
  if (!metadata.isFile || metadata.size !== file.bytes) {
    throw new Error(`ACP runtime file size changed: ${file.path}`);
  }
  const bytes = await Deno.readFile(canonicalPath);
  const digest = toHex(await crypto.subtle.digest("SHA-256", bytes));
  if (digest !== file.sha256) {
    throw new Error(`ACP runtime file digest changed: ${file.path}`);
  }
  fileDigests.set(file.path, digest);
}

for (const adapter of manifest.adapters) {
  const digest = fileDigests.get(adapter.entrypoint);
  if (digest !== adapter.entrypoint_sha256) {
    throw new Error(`ACP entrypoint digest is missing: ${adapter.provider_id}`);
  }
  const command = new Deno.Command(deno, {
    args: [
      "run",
      "--cached-only",
      "--no-config",
      "--node-modules-dir=manual",
      "-A",
      join(root, adapter.entrypoint),
      "--version",
    ],
    cwd: dirname(join(root, adapter.entrypoint)),
    env: { DENO_NO_UPDATE_CHECK: "1", DENO_NO_PROMPT: "1" },
    clearEnv: true,
    stdout: "piped",
    stderr: "piped",
  });
  const output = await command.output();
  const stdout = new TextDecoder().decode(output.stdout).trim();
  if (!output.success || !stdout.includes(adapter.version)) {
    const stderr = new TextDecoder().decode(output.stderr).trim();
    throw new Error(
      `${adapter.provider_id} offline probe failed: ${stdout || stderr}`,
    );
  }
}

console.log(
  `verified ${manifest.adapters.length} offline ACP adapters and ${manifest.files.length} files`,
);

function validManifestPath(path: string): boolean {
  if (path.length === 0 || isAbsolute(path) || path.includes("\\")) {
    return false;
  }
  return path.split("/").every((segment) =>
    segment.length > 0 && segment !== "." && segment !== ".."
  );
}

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
