import { dirname, fromFileUrl, join, relative } from "@std/path";

export interface WebRendererFile {
  path: string;
  bytes: number;
  sha256: string;
}

export interface WebRendererManifest {
  schema_version: 1;
  product: "hyper-term-web-renderers";
  version: string;
  host_contract: {
    native_sdk_web_target: false;
    terminal_requires_gateway: true;
    workbench_standalone_demo: true;
    authoritative_operations_require_rust: true;
  };
  entries: {
    launcher: "index.html";
    terminal: "terminal/index.html";
    workbench: "workbench/index.html";
    compiler_wasm: "workbench/esbuild.wasm";
  };
  files: WebRendererFile[];
}

export function buildWebRendererManifest(
  version: string,
  files: WebRendererFile[],
): WebRendererManifest {
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    throw new Error(`invalid Hyper Term version: ${version}`);
  }
  const sorted = [...files].sort((left, right) =>
    left.path.localeCompare(right.path)
  );
  const paths = new Set(sorted.map((file) => file.path));
  for (
    const required of [
      "README.md",
      "index.html",
      "terminal/index.html",
      "terminal/build-manifest.json",
      "workbench/index.html",
      "workbench/build-manifest.json",
      "workbench/compiler.worker.js",
      "workbench/esbuild.wasm",
      "workbench/genui/preview.html",
    ]
  ) {
    if (!paths.has(required)) {
      throw new Error(`web renderer package is missing ${required}`);
    }
  }
  return {
    schema_version: 1,
    product: "hyper-term-web-renderers",
    version,
    host_contract: {
      native_sdk_web_target: false,
      terminal_requires_gateway: true,
      workbench_standalone_demo: true,
      authoritative_operations_require_rust: true,
    },
    entries: {
      launcher: "index.html",
      terminal: "terminal/index.html",
      workbench: "workbench/index.html",
      compiler_wasm: "workbench/esbuild.wasm",
    },
    files: sorted,
  };
}

const repository = fromFileUrl(new URL("../", import.meta.url));
const output = join(repository, "dist", "web-renderers");

if (import.meta.main) {
  const version = await productVersion(repository);
  await Deno.remove(output, { recursive: true }).catch((error) => {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  });
  await Deno.mkdir(output, { recursive: true });
  await copyTree(
    join(repository, "dist", "terminal"),
    join(output, "terminal"),
  );
  await copyTree(
    join(repository, "dist", "workbench"),
    join(output, "workbench"),
  );
  await Deno.writeTextFile(join(output, "index.html"), webIndex(version));
  await Deno.writeTextFile(join(output, "README.md"), webReadme(version));

  const files = await inventory(output);
  const manifest = buildWebRendererManifest(version, files);
  await Deno.writeTextFile(
    join(output, "manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  console.log(
    `Hyper Term Web Renderer Kit ${version}: ${files.length} verified files`,
  );
}

async function productVersion(root: string): Promise<string> {
  const cargo = await Deno.readTextFile(join(root, "Cargo.toml"));
  const native = await Deno.readTextFile(
    join(root, "apps", "desktop", "app.zon"),
  );
  const cargoVersion = cargo.match(
    /\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
  )?.[1];
  const nativeVersion = native.match(/\.version\s*=\s*"([^"]+)"/)?.[1];
  if (!cargoVersion || cargoVersion !== nativeVersion) {
    throw new Error(
      `Cargo and Native application versions differ: ${cargoVersion} / ${nativeVersion}`,
    );
  }
  return cargoVersion;
}

async function copyTree(source: string, destination: string): Promise<void> {
  const sourceInfo = await Deno.lstat(source).catch((error) => {
    if (error instanceof Deno.errors.NotFound) {
      throw new Error(`built renderer is missing: ${source}`);
    }
    throw error;
  });
  if (!sourceInfo.isDirectory || sourceInfo.isSymlink) {
    throw new Error(`renderer source must be a real directory: ${source}`);
  }
  await Deno.mkdir(destination, { recursive: true });
  for await (const entry of Deno.readDir(source)) {
    const sourcePath = join(source, entry.name);
    const destinationPath = join(destination, entry.name);
    const info = await Deno.lstat(sourcePath);
    if (info.isSymlink) {
      throw new Error(`renderer package rejects symbolic links: ${sourcePath}`);
    }
    if (info.isDirectory) {
      await copyTree(sourcePath, destinationPath);
    } else if (info.isFile) {
      await Deno.mkdir(dirname(destinationPath), { recursive: true });
      await Deno.copyFile(sourcePath, destinationPath);
    } else {
      throw new Error(`renderer package rejects special files: ${sourcePath}`);
    }
  }
}

async function inventory(root: string): Promise<WebRendererFile[]> {
  const files: WebRendererFile[] = [];
  for await (const path of walkFiles(root)) {
    if (relative(root, path) === "manifest.json") continue;
    const bytes = await Deno.readFile(path);
    files.push({
      path: relative(root, path),
      bytes: bytes.byteLength,
      sha256: toHex(await crypto.subtle.digest("SHA-256", bytes)),
    });
  }
  return files;
}

async function* walkFiles(root: string): AsyncGenerator<string> {
  for await (const entry of Deno.readDir(root)) {
    const path = join(root, entry.name);
    const info = await Deno.lstat(path);
    if (info.isSymlink) {
      throw new Error(`renderer package rejects symbolic links: ${path}`);
    }
    if (info.isDirectory) yield* walkFiles(path);
    else if (info.isFile) yield path;
    else throw new Error(`renderer package rejects special files: ${path}`);
  }
}

function toHex(buffer: ArrayBuffer): string {
  return [...new Uint8Array(buffer)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function webIndex(version: string): string {
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <meta
      http-equiv="Content-Security-Policy"
      content="default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; connect-src 'self'; navigate-to 'self'"
    />
    <title>Hyper Term Web Renderers</title>
    <style>
      :root {
        color-scheme: dark light;
        --background: #0d0f0b;
        --surface: #12150f;
        --surface-subtle: #181c15;
        --text: #e6e9dd;
        --muted: #89917e;
        --border: #292f24;
        --accent: #d7ff72;
        --accent-text: #11140d;
        --focus: #a8d558;
      }
      * { box-sizing: border-box; }
      body {
        margin: 0;
        min-height: 100vh;
        display: grid;
        place-items: center;
        padding: 24px;
        background: var(--background);
        color: var(--text);
        font: 14px/1.5 Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      }
      main { width: min(820px, 100%); }
      .eyebrow {
        margin: 0 0 8px;
        color: var(--accent);
        font-size: 12px;
        font-weight: 650;
        letter-spacing: 0.08em;
        text-transform: uppercase;
      }
      h1 { margin: 0; font-size: clamp(28px, 7vw, 40px); line-height: 1.1; }
      .intro { max-width: 620px; color: var(--muted); }
      .surfaces {
        display: grid;
        grid-template-columns: repeat(2, minmax(0, 1fr));
        gap: 12px;
        margin-top: 24px;
      }
      article {
        display: flex;
        min-height: 190px;
        flex-direction: column;
        padding: 16px;
        border: 1px solid var(--border);
        border-radius: 8px;
        background: var(--surface);
      }
      article p { color: var(--muted); }
      code { font-family: SFMono-Regular, Consolas, monospace; }
      a {
        width: fit-content;
        margin-top: auto;
        padding: 8px 12px;
        border-radius: 6px;
        background: var(--accent);
        color: var(--accent-text);
        font-weight: 650;
        text-decoration: none;
      }
      a:focus-visible { outline: 2px solid var(--focus); outline-offset: 3px; }
      footer { margin-top: 16px; color: var(--muted); font-size: 12px; }
      @media (prefers-color-scheme: light) {
        :root {
          --background: #f7f9f1;
          --surface: #ffffff;
          --surface-subtle: #eef2e5;
          --text: #171a14;
          --muted: #626a5b;
          --border: #d5dcc9;
          --accent: #456109;
          --accent-text: #f7ffd9;
          --focus: #5c7d10;
        }
      }
      @media (max-width: 640px) {
        body { padding: 16px; place-items: start center; }
        .surfaces { grid-template-columns: 1fr; }
        article { min-height: 0; }
      }
    </style>
  </head>
  <body>
    <main>
      <p class="eyebrow">Web Renderer Kit · ${version}</p>
      <h1>Terminal when you want it. Agentic UI when you need it.</h1>
      <p class="intro">
        These are the same responsive browser surfaces embedded by the Native
        SDK macOS application. The Workbench runs its TSX compiler in
        WebAssembly; authoritative PTY and agent operations stay with a Rust host.
      </p>
      <section class="surfaces" aria-label="Hyper Term web surfaces">
        <article>
          <h2>Agentic UI Workbench</h2>
          <p>Edit TSX on the left and inspect the isolated live Preview on the right. The included demo runs without a host.</p>
          <a href="workbench/index.html">Open Workbench</a>
        </article>
        <article>
          <h2>Terminal renderer</h2>
          <p>The xterm renderer connects to Hyper Term's token-bound Rust PTY gateway. It intentionally has no browser-side process authority.</p>
          <a href="terminal/index.html">Open renderer</a>
        </article>
      </section>
      <footer>See <code>manifest.json</code> for entries, host requirements, sizes, and SHA-256 digests.</footer>
    </main>
  </body>
</html>
`;
}

function webReadme(version: string): string {
  return `# Hyper Term Web Renderer Kit ${version}

This archive contains the browser surfaces used by the Hyper Term Native SDK
desktop application:

- \`workbench/\`: a standalone responsive Agentic UI demo with editable TSX,
  Diff, an isolated Preview, and the bundled \`esbuild.wasm\` compiler;
- \`terminal/\`: the xterm renderer, which requires Hyper Term's authenticated
  Rust PTY/WebSocket gateway;
- \`manifest.json\`: the host contract and SHA-256 inventory.

Serve this directory over HTTP and open \`index.html\`. Do not open it through a
\`file:\` URL because Worker and WebAssembly loading require an HTTP origin.

This is not a Native SDK Web target. Native SDK 0.5.3 does not provide one.
Hyper Term shares its Design System and browser renderer contracts across the
Native shell and this Deno-built Web/WASM kit. PTYs, ACP/MCP sessions,
permissions, workspace writes, and accepted artifacts remain Rust-owned.
`;
}
