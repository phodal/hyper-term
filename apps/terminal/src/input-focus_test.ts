import { assertEquals } from "@std/assert";
import { TerminalInputFocusLease } from "./input-focus.ts";

Deno.test("terminal focus lease restores only the current input owner", () => {
  let terminalFocuses = 0;
  const lease = new TerminalInputFocusLease(() => terminalFocuses += 1);

  assertEquals(lease.owner, "terminal");
  assertEquals(lease.restore(), true);
  assertEquals(terminalFocuses, 1);

  lease.claimSearch();
  assertEquals(lease.owner, "search");
  assertEquals(lease.restore(), false);
  assertEquals(terminalFocuses, 1);

  lease.claimTerminal();
  assertEquals(lease.owner, "terminal");
  assertEquals(terminalFocuses, 2);
});

Deno.test("repeated search claims never leak focus back to the terminal", () => {
  let terminalFocuses = 0;
  const lease = new TerminalInputFocusLease(() => terminalFocuses += 1);

  lease.claimSearch();
  lease.claimSearch();
  lease.restore();

  assertEquals(terminalFocuses, 0);
});
