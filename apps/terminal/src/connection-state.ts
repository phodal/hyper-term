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
