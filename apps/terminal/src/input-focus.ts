export type TerminalInputOwner = "terminal" | "search";

/**
 * Keeps the terminal grid and its local search field from competing for the
 * same keyboard/IME stream. Native owns focus between app surfaces; this
 * lease owns focus inside the Terminal WebView.
 */
export class TerminalInputFocusLease {
  #owner: TerminalInputOwner = "terminal";
  readonly #focusTerminal: () => void;

  constructor(focusTerminal: () => void) {
    this.#focusTerminal = focusTerminal;
  }

  get owner(): TerminalInputOwner {
    return this.#owner;
  }

  claimTerminal(): void {
    this.#owner = "terminal";
    this.#focusTerminal();
  }

  claimSearch(): void {
    this.#owner = "search";
  }

  restore(): boolean {
    if (this.#owner !== "terminal") return false;
    this.#focusTerminal();
    return true;
  }
}
