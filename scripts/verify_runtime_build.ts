const root = new URL("../dist/runtime/", import.meta.url);
const required = ["genui-compiler.js", "esbuild.wasm", "genui/preview.html"];
const files: Array<{ path: string; bytes: number; sha256: string }> = [];

for (const path of required) {
  const bytes = await Deno.readFile(new URL(path, root));
  if (bytes.byteLength === 0) {
    throw new Error(`runtime asset is empty: ${path}`);
  }
  files.push({
    path,
    bytes: bytes.byteLength,
    sha256: toHex(await crypto.subtle.digest("SHA-256", bytes)),
  });
  if (
    path === "genui/preview.html" &&
    (!new TextDecoder().decode(bytes).includes(
      "HYPER_TERM_ARTIFACT_BOOTSTRAP",
    ) ||
      !new TextDecoder().decode(bytes).includes("hyper_term_preview_boot"))
  ) {
    throw new Error(
      "runtime preview capsule is missing its bootstrap contract",
    );
  }
}
await Deno.writeTextFile(
  new URL("build-manifest.json", root),
  `${
    JSON.stringify(
      {
        schema_version: 1,
        runtime: { name: "deno", version: Deno.version.deno },
        protocol_version: 1,
        files,
      },
      null,
      2,
    )
  }\n`,
);

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
