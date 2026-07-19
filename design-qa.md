# Agent timeline and composer design QA

Date: 2026-07-20

## Visual truth

- Codex.app activity density and disclosure reference:
  `/var/folders/l8/7nm25zns5qjds0jc7z8f1gpm0000gn/T/codex-clipboard-5de94213-8590-43f2-a6d9-d2b382670045.png`
- Codex.app pinned Goal and flexible composer reference:
  `/var/folders/l8/7nm25zns5qjds0jc7z8f1gpm0000gn/T/codex-clipboard-1de50353-17ee-41c2-8f29-5d590789c7e2.png`
- Side-by-side comparison: [Codex reference vs Hyper Term](docs/assets/design-qa/codex-reference-vs-hyper-term.png)

Hyper Term intentionally retains its terminal-first, full-width transcript and
48-point session bar. The adopted Codex.app behaviors are the low-chrome
activity disclosures, a pinned compact Goal above the composer, and
low-frequency Agent controls inside the composer toolbar.

## Rendered implementation

- [Agent at 1022 x 781](docs/assets/design-qa/hyper-term-agent-compact-1022x781.png)
- [Skills menu at 1022 x 781](docs/assets/design-qa/hyper-term-agent-skills-1022x781.png)
- [Agent at the 840 x 520 minimum](docs/assets/design-qa/hyper-term-agent-compact-840x520.png)

The images were captured from the real Native SDK GPU surface with an
authenticated ACP fixture. The rendered state includes an Agent response, two
consecutive command calls, a two-step Goal, the dynamic model selector, and an
ACP-provided `skills` command.

## Comparison passes

### Pass 1: full view

- Activity no longer uses large cards. Two consecutive execute calls collapse
  to the single `Ran 2 commands` disclosure, matching the reference hierarchy.
- Goal is a single collapsed row immediately above the composer instead of a
  permanent right panel or large plan card.
- The composer remains 68 points high for a one-line prompt. Provider controls
  occupy its footer and the Skills command stays behind the plus menu.
- Native controls use the existing Hyper Term dark tokens, borders, typography,
  and icon family; no new decorative surface or fake asset was introduced.

### Pass 2: focused interactions

- Skills opens as an anchored native menu and remains inside the 1022-point
  window bounds.
- Command and Goal disclosures expose semantic toggle actions and start
  collapsed.
- Terminal and Agent tabs retain close controls and the session actions remain
  separate from provider configuration.

### Pass 3: viewport resilience

- At the 840 x 520 minimum, the session bar ends at x=824, the composer spans
  x=8..832, and Send ends at x=824. No control overlaps or clips.
- The command disclosure and Goal both remain visible and collapsed; the
  composer preserves a usable prompt field and toolbar.
- Automation reported zero dispatch errors and 70/1024 retained widget nodes in
  the expanded Skills state.

## Findings and fixes

- P1 fixed: Debug builds enabled whole-document runtime markup reload, which
  bypassed the Zig `rootView` composition seam and left the builder-owned Agent
  timeline blank. Runtime markup reload is now disabled until Native SDK offers
  fragment composition; Debug and Release builds render the same Agent tree.
- No remaining P1 or P2 layout, clipping, interaction, or accessibility issue
  was found in the validated states.

## Final result

Passed for the compact Agent timeline, Goal, composer, Skills menu, and minimum
desktop viewport states described above.
