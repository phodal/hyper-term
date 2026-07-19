import { assertEquals } from "@std/assert";
import {
  installGpuRenderer,
  type TerminalRendererBackend,
} from "./terminal-renderer.ts";

class FakeGpuRenderer {
  disposed = false;
  subscriptionDisposed = false;
  contextLoss?: () => void;

  activate() {}

  onContextLoss(listener: () => void) {
    this.contextLoss = listener;
    return {
      dispose: () => this.subscriptionDisposed = true,
    };
  }

  dispose() {
    this.disposed = true;
  }
}

Deno.test("terminal uses WebGL and falls back after context loss", () => {
  const addon = new FakeGpuRenderer();
  const backends: TerminalRendererBackend[] = [];
  const backend = installGpuRenderer(
    { loadAddon() {} },
    addon,
    (next) => backends.push(next),
  );

  assertEquals(backend, "webgl");
  assertEquals(backends, ["webgl"]);
  addon.contextLoss?.();
  assertEquals(addon.disposed, true);
  assertEquals(addon.subscriptionDisposed, true);
  assertEquals(backends, ["webgl", "dom"]);
});

Deno.test("terminal keeps the DOM renderer when WebGL activation fails", () => {
  const addon = new FakeGpuRenderer();
  const backends: TerminalRendererBackend[] = [];
  const backend = installGpuRenderer(
    {
      loadAddon: () => {
        throw new Error("WebGL2 unavailable");
      },
    },
    addon,
    (next) => backends.push(next),
  );

  assertEquals(backend, "dom");
  assertEquals(addon.disposed, true);
  assertEquals(addon.subscriptionDisposed, true);
  assertEquals(backends, ["dom"]);
});
