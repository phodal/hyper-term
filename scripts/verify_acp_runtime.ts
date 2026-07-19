import { dirname, fromFileUrl, join } from "@std/path";

const deno = Deno.execPath();
const root = fromFileUrl(new URL("../dist/runtime/acp/", import.meta.url));
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

for (const adapter of manifest.adapters) {
  const file = manifest.files.find((candidate) =>
    candidate.path === adapter.entrypoint
  );
  if (!file || file.sha256 !== adapter.entrypoint_sha256) {
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
