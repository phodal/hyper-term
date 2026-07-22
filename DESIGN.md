---
version: alpha
name: Hyper Term Quiet Phosphor
description: A terminal-first desktop system with restrained phosphor accents and explicit agent attention states.
colors:
  primary: "#D7FF72"
  background-dark: "#0D0F0B"
  surface-dark: "#12150F"
  surface-subtle-dark: "#181C15"
  surface-pressed-dark: "#242B1D"
  text-dark: "#E6E9DD"
  text-muted-dark: "#89917E"
  border-dark: "#292F24"
  accent-dark: "#D7FF72"
  accent-text-dark: "#11140D"
  focus-dark: "#A8D558"
  success-dark: "#9BCF5D"
  warning-dark: "#F0BF68"
  destructive-dark: "#FF8D83"
  info-dark: "#8BC6FF"
  background-light: "#F7F9F1"
  surface-light: "#FFFFFF"
  surface-subtle-light: "#EEF2E5"
  surface-pressed-light: "#E1E7D4"
  text-light: "#171A14"
  text-muted-light: "#626A5B"
  border-light: "#D5DCC9"
  accent-light: "#456109"
  accent-text-light: "#F7FFD9"
  focus-light: "#5C7D10"
  success-light: "#37670D"
  warning-light: "#8A5500"
  destructive-light: "#A6312B"
  info-light: "#185E8B"
typography:
  display:
    fontFamily: Inter
    fontSize: 40px
    fontWeight: 650
    lineHeight: 1.1
    letterSpacing: -0.02em
  heading:
    fontFamily: Inter
    fontSize: 24px
    fontWeight: 650
    lineHeight: 1.25
    letterSpacing: -0.01em
  title:
    fontFamily: Inter
    fontSize: 18px
    fontWeight: 600
    lineHeight: 1.3
  body:
    fontFamily: Inter
    fontSize: 14px
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: Inter
    fontSize: 12px
    fontWeight: 600
    lineHeight: 1.3
  terminal:
    fontFamily: SFMono-Regular
    fontSize: 13px
    fontWeight: 400
    lineHeight: 1.45
    fontFeature: "'liga' 1, 'calt' 1"
spacing:
  xs: 4px
  sm: 8px
  md: 12px
  lg: 16px
  xl: 24px
  terminal-cell-x: 8px
  terminal-cell-y: 4px
rounded:
  sm: 4px
  md: 6px
  lg: 8px
  xl: 12px
  full: 9999px
components:
  terminal-surface:
    backgroundColor: "{colors.background-dark}"
    textColor: "{colors.text-dark}"
    typography: "{typography.terminal}"
    padding: "{spacing.lg}"
    rounded: "0px"
  session-tab:
    backgroundColor: "{colors.surface-dark}"
    textColor: "{colors.text-dark}"
    rounded: "{rounded.sm}"
  session-tab-active:
    backgroundColor: "{colors.surface-pressed-dark}"
    textColor: "{colors.accent-dark}"
    rounded: "{rounded.sm}"
  terminal-muted:
    backgroundColor: "{colors.background-dark}"
    textColor: "{colors.text-muted-dark}"
    typography: "{typography.terminal}"
  agent-attention:
    backgroundColor: "{colors.warning-dark}"
    textColor: "{colors.background-dark}"
    rounded: "{rounded.md}"
  approval-primary:
    backgroundColor: "{colors.accent-dark}"
    textColor: "{colors.accent-text-dark}"
    rounded: "{rounded.md}"
  focus-ring:
    backgroundColor: "{colors.focus-dark}"
  status-success:
    backgroundColor: "{colors.success-dark}"
    textColor: "{colors.background-dark}"
    rounded: "{rounded.full}"
  status-destructive:
    backgroundColor: "{colors.destructive-dark}"
    textColor: "{colors.background-dark}"
    rounded: "{rounded.full}"
  status-info:
    backgroundColor: "{colors.info-dark}"
    textColor: "{colors.background-dark}"
    rounded: "{rounded.full}"
  block-surface:
    backgroundColor: "{colors.surface-dark}"
    textColor: "{colors.text-dark}"
    rounded: "{rounded.lg}"
    padding: "{spacing.md}"
  block-surface-subtle:
    backgroundColor: "{colors.surface-subtle-dark}"
    textColor: "{colors.text-muted-dark}"
    rounded: "{rounded.lg}"
  separator:
    backgroundColor: "{colors.border-dark}"
    height: "1px"
  brand-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.accent-text-dark}"
  light-terminal-surface:
    backgroundColor: "{colors.background-light}"
    textColor: "{colors.text-light}"
    typography: "{typography.terminal}"
    padding: "{spacing.lg}"
  light-surface:
    backgroundColor: "{colors.surface-light}"
    textColor: "{colors.text-light}"
    rounded: "{rounded.lg}"
  light-surface-subtle:
    backgroundColor: "{colors.surface-subtle-light}"
    textColor: "{colors.text-muted-light}"
    rounded: "{rounded.lg}"
  light-surface-pressed:
    backgroundColor: "{colors.surface-pressed-light}"
    textColor: "{colors.text-light}"
  light-separator:
    backgroundColor: "{colors.border-light}"
    height: "1px"
  light-primary:
    backgroundColor: "{colors.accent-light}"
    textColor: "{colors.accent-text-light}"
    rounded: "{rounded.md}"
  light-focus-ring:
    backgroundColor: "{colors.focus-light}"
  light-status-success:
    backgroundColor: "{colors.success-light}"
    textColor: "{colors.surface-light}"
    rounded: "{rounded.full}"
  light-agent-attention:
    backgroundColor: "{colors.warning-light}"
    textColor: "{colors.surface-light}"
    rounded: "{rounded.md}"
  light-status-destructive:
    backgroundColor: "{colors.destructive-light}"
    textColor: "{colors.surface-light}"
    rounded: "{rounded.full}"
  light-status-info:
    backgroundColor: "{colors.info-light}"
    textColor: "{colors.surface-light}"
    rounded: "{rounded.full}"
---

# Hyper Term Design System

## Overview

Hyper Term should disappear when the user wants a terminal and become
deliberately visible only when an agent needs attention. The visual character is
quiet, dense, precise, and slightly phosphorescent. Default Terminal sessions
must not look like a chat product; Agent sessions add semantic Blocks around the
same terminal rather than replacing it.

Native and Web/WASM renderers share semantic tokens and state names. They may
use platform-appropriate controls, but color meaning, spacing rhythm, focus,
approval hierarchy, and attention levels must remain equivalent.

## Colors

Dark mode is the signature environment: near-black olive surfaces preserve the
feel of a terminal without crushing contrast. Acid phosphor is the single
identity accent and is reserved for focus, active sessions, and primary safe
actions. Amber means human attention, red means destructive or failed, blue is
informational, and green means verified success.

Light mode is a first-class accessibility and daylight palette, not an inverted
afterthought. Both palettes keep body text above a 4.5:1 contrast ratio. High
contrast mode may replace brand colors with stronger system-derived values.

## Typography

Interface copy uses Inter where bundled and the platform sans fallback
otherwise. Terminal cells use the system monospace stack headed by SF Mono on
macOS. Shell output, diffs, code, paths, commands, and protocol identifiers are
monospace; explanations and controls remain sans serif.

Avoid oversized chat typography. Terminal output stays at 13px by default with
user-controlled zoom. Agent Blocks use the 14px body rung and reserve larger
type for empty states or session-level summaries.

## Layout

The base rhythm is 4px, with 8, 12, 16, and 24px composition steps. A Terminal
session gives nearly all available space to its cell grid. Persistent chrome is
limited to the integrated title/session row and a compact status row.

Agent mode composes a resizable terminal pane and a semantic Block pane. At
narrow widths the product must preserve the terminal and collapse secondary
Agent navigation before clipping commands or terminal cells. WebView islands
mount only in visible Block slots and never become the whole application shell.

## Elevation & Depth

Hierarchy comes from tonal surfaces and one-pixel separators. Shadows are
reserved for floating menus, dialogs, and WebView islands that genuinely sit
above native content. Terminal output, transcripts, and ordinary Blocks remain
flat to maximize density and reduce visual noise.

## Shapes

Controls use restrained 4-8px radii; terminal and editor surfaces meet pane
edges without decorative rounding. Pills are limited to compact statuses. The
shape language must remain engineered and native, not bubbly or card-heavy.

## Components

- The terminal surface owns text selection, cursor, IME, hyperlinks,
  accessibility, alternate screen, and scrollback. It never renders inside the
  JSON WebView bridge.
- A session tab shows mode only when needed. Ordinary Terminal is the default;
  Agent is an explicit choice in New Session.
- Agent Blocks use stable semantic kinds such as message, plan, tool, approval,
  diff, artifact, and Computer Use evidence. An agent supplies data, never
  trusted native markup.
- Approval controls always state the proposed effect, scope, and risk. The
  primary action may use the accent only when it is the safe recommended path;
  destructive authorization stays red.
- Approval detail is Rust-authenticated Native chrome, never provider prose or
  WebView markup. Keep argv items distinct, redact credential material, show
  cwd and effective capabilities, and bind authorization to both the detail
  digest and operation revision. If the bounded detail cannot be projected,
  disable Allow while preserving Reject and Cancel.
- Attention uses progressive disclosure: passive status, badge, native
  notification, then urgent system attention. Repeated animation is never the
  sole signal. Native notifications are background-only, deduplicated by the
  Rust-projected Agent state, and limited to approval, review-ready, and failure
  transitions; Terminal output and provider prose are never notification input.
- Generated React UI is isolated in a bounded WebView Block with explicit
  origin, capability, focus, and lifetime leases.

## Do's and Don'ts

- Do open into a fast, familiar zsh Terminal with no mandatory agent chrome.
- Do keep keyboard-first workflows, native menus, shortcuts, focus rings, and
  reduced-motion behavior.
- Do show durable operation history separately from raw shell transcript.
- Do preserve terminal state when the renderer, WebView, Deno, or agent reloads.
- Don't turn every command into a chat bubble or card.
- Don't route PTY bytes, model token streams, or compiled artifacts through the
  WebView JSON bridge.
- Don't let Zig, React, Deno, ACP, MCP, or generated UI bypass the Rust
  permission broker.
- Don't use color alone for approval, failure, or attention state.
