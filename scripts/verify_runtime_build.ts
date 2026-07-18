const root = new URL("../dist/runtime/", import.meta.url);
const required = ["genui-compiler.js", "esbuild.wasm"];
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
