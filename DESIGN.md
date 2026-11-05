---
version: alpha
name: "DeepSeek Harness for Linux"
description: "A quiet startup and recovery surface around the original DeepSeek Harness workspace."
colors:
  background: "#f7f9fc"
  text: "#18243b"
  muted: "#52617a"
  primary: "#315fd8"
  primary-hover: "#244cb7"
  border: "#ccd5e4"
  danger: "#a62c36"
  scroll-thumb: "#8a9ab4"
  scroll-track: "#e8edf5"
typography:
  sans:
    fontFamily: 'Ubuntu, Cantarell, "Segoe UI", sans-serif'
  display:
    fontFamily: 'Ubuntu, Cantarell, "Segoe UI", sans-serif'
rounded:
  DEFAULT: "12px"
spacing:
  section-gap: "24px"
  page-max: "620px"
components:
  button: {}
  status: {}
---

# DeepSeek Harness for Linux Design System

## Overview

### Creative North Star

An Ubuntu application opening a working notebook: calm, legible, and brief. The
startup screen hands over to the upstream interface as soon as it is ready.

### Product context and register

- Audience and primary job: this user's Ubuntu desktop; open a local Harness workspace.
- Market: personal desktop use, inferred from the current request; no regulated or Japan-market scope.
- Language: owned shell copy is English; the upstream workspace controls its own language.
- Register: product utility, with no marketing or new chat interface.
- Signature: a small blue `h↗` mark expressing opening a workspace.
- Restraint: preserve upstream chat, model configuration, permissions and workspace UI.
- Anti-references: promotional landing pages, invented agent dashboards, or a second settings system.
- Source evidence: the user's request and upstream Web/Desktop architecture, linked in README.md.
- Runtime ownership: `web/styles.css` is canonical; this document mirrors its values.
  Colors map directly from the frontmatter to same-named CSS variables. Radius maps
  to `--radius`, section-gap to `--space`; page-max is the main width.

## Colors

The light palette is a cool paper surface, dark slate text and DeepSeek-adjacent
blue. Red indicates a recoverable startup error, accompanied by written guidance.
Dark mode adapts these CSS variables in one stylesheet: background #151b27, text
#e6edf9, muted #a7b7d0, primary #8babff, hover #a7beff, border #3d4b63, danger
#ff8891, thumb #5d708e, track #202a3a. Forced colors preserve system controls.

## Typography

Use Ubuntu/Cantarell for the owned desktop surface, with system fallbacks and no
remote fonts. The display role uses 32–48px at weight 500 with gentle -0.015em
letter spacing; the app mark uses normal spacing. Copy uses the system
baseline with 1.5–1.7 line height. The 12px footer is secondary attribution only.
The upstream application's typography is not modified.

## Layout

The shell has one centered content column, at most 620px wide. Main padding is
64px/32px, reduced to 32px/24px below 480px. The body owns scrolling. Error text
wraps without truncation; no success or error message overlays a control.

## Elevation & Depth

No cards, overlays or shadows. Text hierarchy and space carry the structure.

## Shapes

The button uses a 12px radius. The 16px loading indicator is circular. The square
app icon uses a restrained rounded corner and the same blue mark.

## Components

### Foundational visual states

Opening shows an app-owned spinner and an accessible status message. Success
replaces the startup page with the existing Harness workspace. Failure shows
persistent inline text and a keyboard-focusable Try again action. Retry disables
the action until it settles. Reduced motion keeps a static indicator.

### Buttons and actions

Try again is the only owned page action. It uses a native button, solid primary
color, distinct hover/pressed state, 44px minimum height and visible focus ring.
The native tray provides Open DeepSeek Harness and Quit DeepSeek Harness. Closing a window hides
it so tasks continue; quitting gracefully stops the owned backend. Reload workspace
repairs the view while preserving a running backend. Opening the app again retries
a stopped workspace and repairs a blank view. Healthy views keep their drafts.
The tray's disabled status row reads Workspace running after backend readiness and
Workspace stopped after a failed start or backend exit. It is informational only.
Workspace startup has a bounded wait and returns to a retry screen if the view
cannot finish opening.

### Navigation and data display

The upstream Web application owns workspace navigation and data. External web
links open in the default browser; only the owned local service replaces the
desktop page. No custom tables or selection controls are introduced.

### Forms and overlays

No owned forms, secret fields, modals or toasts. API keys are entered using the
original Harness settings. Native desktop commands are restricted to the bundled
startup page, not the local runtime or external sites.

### Iconography

The original code-native h/arrow mark identifies this independent shell. Loading
and error indicators always have accompanying text; decoration is aria-hidden.

### Motion

Only the indeterminate spinner rotates (one second, linear). No page transition
animation or simulated percentage is used. Reduced motion disables animation.

### Content and data visualization

Plain English names the next action. Diagnostics never show the private startup
URL or tokens. Timing measurements are local development evidence, not UI claims.

## Do's and Don'ts

- Do preserve upstream workspace behavior and visual identity.
- Do provide bounded startup and a real retry path.
- Don't imply that the independent shell is an official DeepSeek release.
- Don't claim cloud model generation is faster because the window uses Rust.

## Canonical UI Map

| Capability | Canonical owner | Source of truth | Allowed variants | Verification |
| --- | --- | --- | --- | --- |
| Scrollbar | `web/styles.css` global baseline | This document and stylesheet | light/dark/forced colors | Browser computed styles |
| Startup feedback | `web/startup.js` and status region | Backend lifecycle result | opening/error/retry | Desktop smoke test |
