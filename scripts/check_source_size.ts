import { join, relative } from "@std/path";

const sourceRoots = ["apps", "crates", "runtime", "scripts"] as const;
const sourceExtensions = new Set([".rs", ".sh", ".ts", ".tsx", ".zig"]);
const ignoredDirectories = new Set([
  ".deno-cache",
  ".git",
  ".zig-cache",
  "dist",
  "node_modules",
  "target",
  "zig-out",
  "zig-pkg",
]);

export const defaultSourceLineLimit = 2_000;

// These files predate the line limit. Freeze their current size so they can
// only shrink while cohesive modules are extracted in follow-up changes.
export const legacySourceLineLimits: Readonly<Record<string, number>> = {
  "apps/desktop/src/main.zig": 5_470,
  "apps/desktop/src/tests.zig": 2_644,
  "crates/hyper-term-daemon/src/agent_gateway.rs": 9_271,
  "crates/hyper-term-daemon/src/desktop.rs": 2_351,
  "crates/hyper-term-daemon/src/lib.rs": 3_951,
  "crates/hyper-term-daemon/src/workspace_apply.rs": 2_924,
  "crates/hyper-term-drivers/src/acp.rs": 2_695,
};

export function sourceLineLimit(path: string): number {
  return legacySourceLineLimits[path] ?? defaultSourceLineLimit;
}

export function sourceLineCount(source: string): number {
  if (source.length === 0) return 0;
  let lines = 0;
  for (const character of source) {
    if (character === "\n") lines += 1;
  }
  return source.endsWith("\n") ? lines : lines + 1;
}

function sourceExtension(path: string): string {
  const slash = path.lastIndexOf("/");
  const dot = path.lastIndexOf(".");
  return dot > slash ? path.slice(dot) : "";
}

async function* sourceFiles(directory: string): AsyncGenerator<string> {
  for await (const entry of Deno.readDir(directory)) {
    if (entry.isSymlink) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory) {
      if (!ignoredDirectories.has(entry.name)) yield* sourceFiles(path);
      continue;
    }
    if (entry.isFile && sourceExtensions.has(sourceExtension(path))) yield path;
  }
}

async function main(): Promise<void> {
  const root = Deno.cwd();
  const violations: string[] = [];
  let checked = 0;
  for (const sourceRoot of sourceRoots) {
    for await (const path of sourceFiles(join(root, sourceRoot))) {
      const repositoryPath = relative(root, path).replaceAll("\\", "/");
      const lines = sourceLineCount(await Deno.readTextFile(path));
      const limit = sourceLineLimit(repositoryPath);
      checked += 1;
      if (lines > limit) {
        violations.push(`${repositoryPath}: ${lines} lines exceeds ${limit}`);
      }
    }
  }

  if (violations.length > 0) {
    violations.sort();
    console.error(
      "Source files must be split at cohesive architecture boundaries:",
    );
    for (const violation of violations) console.error(`- ${violation}`);
    Deno.exit(1);
  }
  console.log(
    `Source size check passed: ${checked} files; new files <= ${defaultSourceLineLimit} lines; legacy hotspots frozen.`,
  );
}

if (import.meta.main) await main();
