const output = new URL("../dist/terminal", import.meta.url);

try {
  await Deno.remove(output, { recursive: true });
} catch (error) {
  if (!(error instanceof Deno.errors.NotFound)) {
    throw error;
  }
}
await Deno.mkdir(output, { recursive: true });
