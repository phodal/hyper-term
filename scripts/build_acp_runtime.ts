import { fromFileUrl, join, relative, resolve, SEPARATOR } from "@std/path";

const repository = resolve(fromFileUrl(new URL("..", import.meta.url)));
const sourceRoot = join(repository, "runtime", "acp");
const source = join(sourceRoot, "node_modules");
const output = join(repository, "dist", "runtime", "acp");
const outputModules = join(output, "node_modules");
const maxRuntimeFiles = 8 * 1024;
const maxRuntimeBytes = 128 * 1024 * 1024;

const adapters = [
  {
    provider_id: "codex-acp",
    package: "@agentclientprotocol/codex-acp",
    version: "1.1.4",
    entrypoint: "node_modules/@agentclientprotocol/codex-acp/dist/index.js",
    required_agent: "codex",
  },
  {
    provider_id: "claude-acp",
    package: "@agentclientprotocol/claude-agent-acp",
    version: "0.59.0",
    entrypoint:
      "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js",
    required_agent: "claude",
  },
] as const;

await Deno.remove(output, { recursive: true }).catch((error) => {
  if (!(error instanceof Deno.errors.NotFound)) throw error;
});
await Deno.mkdir(outputModules, { recursive: true });
await copyProductionTree(source, outputModules);
for (const provenance of ["deno.lock", "package.json"]) {
  await Deno.copyFile(join(sourceRoot, provenance), join(output, provenance));
}

const files: Array<{ path: string; bytes: number; sha256: string }> = [];
for await (const path of regularFiles(output)) {
  const bytes = await Deno.readFile(path);
  files.push({
    path: relative(output, path).split(SEPARATOR).join("/"),
    bytes: bytes.byteLength,
    sha256: toHex(await crypto.subtle.digest("SHA-256", bytes)),
  });
}
files.sort((left, right) => left.path.localeCompare(right.path));
const runtimeBytes = files.reduce((total, file) => total + file.bytes, 0);
if (files.length > maxRuntimeFiles || runtimeBytes > maxRuntimeBytes) {
  throw new Error(
    `ACP runtime exceeds its release budget: ${files.length} files, ${runtimeBytes} bytes`,
  );
}

const resolvedAdapters = [];
for (const adapter of adapters) {
  const file = files.find((candidate) => candidate.path === adapter.entrypoint);
  if (!file) {
    throw new Error(`ACP entrypoint is missing: ${adapter.entrypoint}`);
  }
  const packageMetadata = JSON.parse(
    await Deno.readTextFile(
      join(outputModules, ...adapter.package.split("/"), "package.json"),
    ),
  );
  if (packageMetadata.version !== adapter.version) {
    throw new Error(
      `${adapter.package} resolved ${packageMetadata.version}, expected ${adapter.version}`,
    );
  }
  resolvedAdapters.push({
    ...adapter,
    entrypoint_sha256: file.sha256,
  });
}

await Deno.writeTextFile(
  join(output, "manifest.json"),
  `${
    JSON.stringify(
      {
        schema_version: 1,
        runtime: { name: "deno", version: Deno.version.deno },
        adapters: resolvedAdapters,
        files,
      },
      null,
      2,
    )
  }\n`,
);

async function copyProductionTree(from: string, to: string): Promise<void> {
  for await (const entry of Deno.readDir(from)) {
    // Deno's manual node_modules layout keeps its pnpm content store and
    // installer metadata beside the production links. Following the links
    // below already materializes a self-contained runtime; copying the store
    // as well duplicates provider binaries and can inflate the app by >1 GiB.
    if (from === source && entry.name.startsWith(".")) {
      continue;
    }
    const destinationScope = relative(outputModules, to).split(SEPARATOR).join(
      "/",
    );
    if (destinationScope === "@openai/codex" && entry.name === "node_modules") {
      continue;
    }
    if (isBundledAgentBinary(entry.name, from)) continue;
    const sourcePath = join(from, entry.name);
    const targetPath = join(to, entry.name);
    const metadata = await Deno.lstat(sourcePath);
    if (metadata.isDirectory) {
      await Deno.mkdir(targetPath, { recursive: true });
      await copyProductionTree(sourcePath, targetPath);
    } else if (metadata.isFile) {
      await Deno.copyFile(sourcePath, targetPath);
    } else if (metadata.isSymlink) {
      const resolved = await Deno.realPath(sourcePath);
      const root = await Deno.realPath(source);
      if (resolved !== root && !resolved.startsWith(`${root}${SEPARATOR}`)) {
        throw new Error(
          `ACP dependency symlink escapes node_modules: ${sourcePath}`,
        );
      }
      const resolvedMetadata = await Deno.stat(resolved);
      if (resolvedMetadata.isDirectory) {
        await Deno.mkdir(targetPath, { recursive: true });
        await copyProductionTree(resolved, targetPath);
      } else {
        await Deno.copyFile(resolved, targetPath);
      }
    }
  }
}

function isBundledAgentBinary(name: string, parent: string): boolean {
  const scope = relative(source, parent).split(SEPARATOR).join("/");
  return (scope === "@openai" && name.startsWith("codex-") &&
    name !== "codex") ||
    (scope === "@anthropic-ai" &&
      name.startsWith("claude-agent-sdk-") && name !== "claude-agent-sdk");
}

async function* regularFiles(root: string): AsyncGenerator<string> {
  for await (const entry of Deno.readDir(root)) {
    const path = join(root, entry.name);
    if (entry.isDirectory) yield* regularFiles(path);
    else if (entry.isFile) yield path;
    else throw new Error(`unexpected ACP runtime symlink after copy: ${path}`);
  }
}

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
