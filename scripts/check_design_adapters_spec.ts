import { assert, assertEquals } from "@std/assert";
import {
  collectDesignAdapterFindings,
  type DesignAdapterSources,
} from "./check_design_adapters.ts";

async function fixture(): Promise<{
  design: string;
  sources: DesignAdapterSources;
}> {
  const [
    design,
    nativeZig,
    terminalCss,
    terminalTs,
    workbenchCss,
    previewHtml,
  ] = await Promise
    .all([
      Deno.readTextFile("DESIGN.md"),
      Deno.readTextFile("apps/desktop/src/main.zig"),
      Deno.readTextFile("apps/terminal/src/styles.css"),
      Deno.readTextFile("apps/terminal/src/terminal-theme.ts"),
      Deno.readTextFile("apps/workbench/src/styles.css"),
      Deno.readTextFile("apps/workbench/genui/preview.html"),
    ]);
  return {
    design,
    sources: {
      nativeZig,
      terminalCss,
      terminalTs,
      workbenchCss,
      previewHtml,
    },
  };
}

Deno.test("DESIGN.md adapters match Native, Workbench, and Terminal", async () => {
  const current = await fixture();
  assertEquals(
    collectDesignAdapterFindings(current.design, current.sources),
    [],
  );
});

Deno.test("DESIGN.md adapter check names CSS and Native drift", async () => {
  const current = await fixture();
  const findings = collectDesignAdapterFindings(current.design, {
    ...current.sources,
    nativeZig: current.sources.nativeZig.replace(
      ".accent = canvas.Color.rgb8(215, 255, 114)",
      ".accent = canvas.Color.rgb8(255, 255, 255)",
    ),
    workbenchCss: current.sources.workbenchCss.replace(
      "--workbench-accent: #d7ff72;",
      "--workbench-accent: #ffffff;",
    ).replace(
      "--workbench-background: #f7f9f1;",
      "--workbench-background: #ffffff;",
    ),
  });
  assert(findings.some((finding) => finding.includes("accent-dark")));
  assert(
    findings.some((finding) => finding.includes("--workbench-accent")),
  );
  assert(findings.some((finding) => finding.includes("background-light")));
});
