const sourceHtml = new URL(
  "../apps/workbench/genui/preview.html",
  import.meta.url,
);
const target = Deno.args[0] ?? "workbench";
if (!/^[a-z0-9_-]+$/.test(target)) {
  throw new Error("preview target must be a bounded build directory name");
}
const outputHtml = new URL(
  `../dist/${target}/genui/preview.html`,
  import.meta.url,
);
const shellBundle = new URL(
  `../dist/${target}/genui/preview-shell.js`,
  import.meta.url,
);
const marker = "<!-- HYPER_TERM_PREVIEW_SHELL -->";

const [template, bundle] = await Promise.all([
  Deno.readTextFile(sourceHtml),
  Deno.readTextFile(shellBundle),
]);
if (!template.includes(marker)) {
  throw new Error("isolated preview template is missing its shell marker");
}

const safeBundle = bundle.replaceAll(/<\/script/gi, "<\\/script");
const document = template.replace(
  marker,
  `<script type="module">\n${safeBundle}\n</script>`,
);
await Deno.writeTextFile(outputHtml, document);
await Deno.remove(shellBundle);
