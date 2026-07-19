import type { ITerminalAddon } from "@xterm/xterm";

export type TerminalRendererBackend = "webgl" | "dom";

interface Disposable {
  dispose(): void;
}

interface GpuRendererAddon extends ITerminalAddon {
  dispose(): void;
  onContextLoss(listener: () => void): Disposable;
}

interface AddonHost {
  loadAddon(addon: ITerminalAddon): void;
}

export function installGpuRenderer<TAddon extends GpuRendererAddon>(
  terminal: AddonHost,
  addon: TAddon,
  onBackendChanged: (backend: TerminalRendererBackend) => void = () => {},
): TerminalRendererBackend {
  let active = true;
  const fallBackToDom = () => {
    if (!active) return;
    active = false;
    contextLossSubscription.dispose();
    addon.dispose();
    onBackendChanged("dom");
  };
  const contextLossSubscription = addon.onContextLoss(fallBackToDom);

  try {
    terminal.loadAddon(addon);
    onBackendChanged("webgl");
    return "webgl";
  } catch {
    fallBackToDom();
    return "dom";
  }
}
