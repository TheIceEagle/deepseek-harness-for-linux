---
version: alpha
name: "DeepSeek Harness Native"
description: "A native Ubuntu conversation workspace with a Rust backend and explicit text extensions."
colors:
  background: "#f7f9fc"
  surface: "#ffffff"
  text: "#18243b"
  muted: "#52617a"
  primary: "#315fd8"
  primary-hover: "#244cb7"
  border: "#dce3ef"
  tint: "#edf2ff"
  danger: "#a62c36"
  scroll-thumb: "#8a9ab4"
  scroll-track: "#e8edf5"
typography:
  sans:
    fontFamily: "Ubuntu, Cantarell, sans-serif"
  display:
    fontFamily: "Ubuntu, Cantarell, sans-serif"
rounded:
  DEFAULT: "8px"
spacing:
  section-gap: "24px"
components:
  button: {}
  message: {}
---

# DeepSeek Harness Native Design System

## Overview

An Ubuntu conversation notebook: a quiet blue navigation rail, generous reading
space, and a stable composer. The signature is the existing h/arrow mark and
blue author labels, rather than decorative agent dashboards. This is a working
product surface for this user's Linux desktop, in English. There is no
Japan-market or regulated-workflow scope.

The current user explicitly selected a native Rust interface and backend. This
new interface has its own contract; the repository's root DESIGN.md continues to
govern the original Tauri startup/recovery surface. Neither the upstream UI nor
the original data profile is restyled or migrated by this prototype.

`crates/app/src/style.css` is the canonical native token source (ownership model
B). This file mirrors its values. GTK owns native control semantics, popup
geometry, focus, text input, file selection, and scrolling mechanics. Shared
app-owned modal behavior lives in `crates/app/src/dialogs.rs`.

## Colors

Cool paper, white conversation surface, dark slate text and a blue primary action
continue the existing shell's identity. Red accompanies a written failure and a
recovery action. Color never identifies a message status on its own. Native CSS
`@define-color` names map background→app_bg, surface→surface, text→ink,
primary→accent, primary-hover→accent_hover, border→line, tint→tint,
scroll-thumb→scroll_thumb, and scroll-track→scroll_track. Other names map directly.

## Typography

Ubuntu/Cantarell system fonts; no network fonts. Window title 14px/600, body
14px, selectable message text 16px, author 13px/700, secondary help 11–12px.
The empty conversation question uses 29px/500 sparingly. Native Markdown uses
weight and monospace tags, not an HTML renderer. Plugin text is never evaluated
as markup. Keep natural letter spacing and text selection.

## Layout

The conversation rail is 220px; the window starts at 1120×800 with a 640×480
minimum. Message rows use 36px horizontal padding and bounded pages of at most
50 native text widgets. Earlier/Latest controls expose the selected message range
without constructing the complete history's widget tree.
The main conversation and sidebar each own their scrolling. The composer owns a
bounded 86–180px input scroller and a stable Send/Stop slot. Notices wrap below
it without covering controls. Settings and extension dialogs remain separate
from conversation scroll ownership.

## Elevation & Depth

Hairline separation, a lightly tinted rail and user-message backgrounds. No
gradients, hero cards, heavy shadows or decorative metrics.

## Shapes

Controls and conversation rows use 8px corners; the composer uses 12px. Native
selection and focus treatments remain visible. Primary buttons have deliberate
hover/active/disabled states.

## Components

See UX-CONTRACT.md for the canonical UI map and behavior ledger. New conversation,
saved-session selection, connection settings and extension installation share
the same inline feedback language. There are no browser dialogs or HTML widgets.

Scrollbars inherit the global GTK CSS thumb/track/hover/active treatment on every
owned scroller. Native controls retain accessible names and keyboard semantics.
Only active work spins; generated text is batched for display every 33ms. A
cancelled or interrupted reply keeps its text and a written status.

## Do's and Don'ts

- Do preserve drafts, text selection, partial replies and saved conversations.
- Do label this as an unofficial native preview with a different plugin format.
- Do show useful empty, pending, cancelled, failure and recovery states.
- Don't persist API keys or show raw provider bodies in errors.
- Don't imply compatibility with arbitrary Cordis/React plugins.
- Don't claim a memory or speed improvement without matching measurements.

## Canonical UI Map

| Capability | Canonical owner | Source of truth | Allowed variants | Verification |
| --- | --- | --- | --- | --- |
| Form | GTK Entry/PasswordEntry/TextView + Settings validation | Core Settings + UX contract | Connection settings | Native GUI smoke |
| Select/Listbox | GTK ListBox and bounded native history pages | UX contract | Sessions/messages | Keyboard and conversation restoration |
| Scrollbar | style.css global GTK baseline | This document | Native geometry | Native screenshots and scroll checks |
| Dialog | dialogs.rs native modal Window | UX contract | Settings/extensions/confirmation | Focus, Escape, file picker |
| Feedback | Ui::status and dialog feedback labels | UX contract | Pending/saved/error/cancelled | Native GUI smoke |
| Tool setup | tools_ui.rs + tools.rs registry | UX contract | MCP server / JavaScript tool plugin | Local fixture desktop smoke |
| Tool approval | Ui::approve_tool + shared modal | UX contract | Allow once / Deny / Stop | Checkpoint-before-dispatch tests |
| Tool history | Ui::tool_activity | Persisted canonical trace | Latest 50 calls, expandable previews | Saved history and restart checks |
