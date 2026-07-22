import { extname, resolve, SEPARATOR } from "@std/path";

const [assetsArgument, readyPath] = Deno.args;
if (!assetsArgument || !readyPath) {
  throw new Error("usage: serve_verification_assets.ts <assets> <ready-file>");
}

const assetsRoot = await Deno.realPath(assetsArgument);
const contentTypes: Readonly<Record<string, string>> = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".wasm": "application/wasm",
};

function assetPath(request: Request): string | undefined {
  let pathname: string;
  try {
    pathname = decodeURIComponent(new URL(request.url).pathname);
  } catch {
    return undefined;
  }
  const relativePath = pathname === "/" ? "index.html" : `.${pathname}`;
  const candidate = resolve(assetsRoot, relativePath);
  if (
    candidate !== assetsRoot &&
    !candidate.startsWith(`${assetsRoot}${SEPARATOR}`)
  ) {
    return undefined;
  }
  return candidate;
}

const server = Deno.serve({
  hostname: "127.0.0.1",
  port: 0,
  onListen: ({ port }) => {
    Deno.writeTextFileSync(readyPath, `http://127.0.0.1:${port}`);
  },
}, async (request) => {
  const path = assetPath(request);
  if (!path) {
    return new Response("Bad Request", { status: 400 });
  }
  try {
    const body = await Deno.readFile(path);
    return new Response(body, {
      headers: {
        "cache-control": "no-store",
        "content-type": contentTypes[extname(path)] ??
          "application/octet-stream",
      },
    });
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) {
      return new Response("Not Found", { status: 404 });
    }
    throw error;
  }
});

await server.finished;
