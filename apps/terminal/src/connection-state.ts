export class TerminalConnectionState {
  #ready = false;
  #inputSequence = 1n;
  #resizeGeneration = 0;

  beginConnection(): void {
    this.#ready = false;
  }

  acceptReady(nextInputSequence: number, resizeGeneration: number): void {
    this.#inputSequence = BigInt(nextInputSequence);
    this.#resizeGeneration = resizeGeneration;
    this.#ready = true;
  }

  disconnect(): void {
    this.#ready = false;
  }

  canSend(socketOpen: boolean): boolean {
    return socketOpen && this.#ready;
  }

  takeInputSequence(): bigint {
    this.requireReady();
    const sequence = this.#inputSequence;
    this.#inputSequence += 1n;
    return sequence;
  }

  takeResizeGeneration(): number {
    this.requireReady();
    this.#resizeGeneration += 1;
    return this.#resizeGeneration;
  }

  private requireReady(): void {
    if (!this.#ready) throw new Error("terminal protocol is not ready");
  }
}

export interface TerminalReconnectPresentation {
  message: string;
  visible: boolean;
}

export function terminalReconnectPresentation(
  hasReachedReady: boolean,
  delayMilliseconds: number,
): TerminalReconnectPresentation {
  if (hasReachedReady) return { message: "Reattaching…", visible: false };
  return {
    message: `Disconnected · retrying in ${
      Math.round(delayMilliseconds / 100) / 10
    }s`,
    visible: true,
  };
}

export function terminalAttachmentStorageKey(locationHref: string): string {
  const base = "hyper-term.terminal-attachment.v1";
  const tab = terminalSessionId(locationHref);
  return tab === null ? base : `${base}.tab-${tab}`;
}

export function terminalSessionId(locationHref: string): number | null {
  const tab = new URL(locationHref).searchParams.get("tab");
  if (!tab || !/^[1-9][0-9]{0,2}$/.test(tab)) return null;
  const value = Number(tab);
  return value <= 999 ? value : null;
}
