interface RuntimeManifest {
  schema_version: number;
  runtime: string;
  version: string;
  tool_protocol_version: number;
  artifacts: Array<{
    target: string;
    sha256: string;
    executable_sha256?: string;
    url: string;
  }>;
}

const path = new URL("../runtime/deno-manifest.json", import.meta.url);
const manifest = JSON.parse(await Deno.readTextFile(path)) as RuntimeManifest;

if (manifest.schema_version !== 1 || manifest.runtime !== "deno") {
  throw new Error("unsupported Deno runtime manifest");
}
if (manifest.version !== Deno.version.deno) {
  throw new Error(
    `runtime manifest pins Deno ${manifest.version}, running ${Deno.version.deno}`,
  );
}
if (manifest.tool_protocol_version !== 1 || manifest.artifacts.length === 0) {
  throw new Error("runtime manifest is missing its protocol or artifacts");
}
for (const artifact of manifest.artifacts) {
  if (!/^[a-f0-9]{64}$/.test(artifact.sha256)) {
    throw new Error(`invalid SHA-256 for ${artifact.target}`);
  }
  if (
    artifact.executable_sha256 !== undefined &&
    !/^[a-f0-9]{64}$/.test(artifact.executable_sha256)
  ) {
    throw new Error(`invalid executable SHA-256 for ${artifact.target}`);
  }
  const url = new URL(artifact.url);
  if (url.protocol !== "https:" || url.hostname !== "github.com") {
    throw new Error(`untrusted runtime URL for ${artifact.target}`);
  }
}
