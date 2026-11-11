# Native client boundary

The native preview is a separate Rust implementation. Its normal operation has
no JavaScript runtime, local HTTP backend, or WebView. Optional external tools
run only after explicit connection. GTK4 supplies native
widgets; its system libraries are not rewritten in Rust.

```text
GTK4 desktop interface
  ├── Rust chat core → HTTPS OpenAI-compatible provider
  ├── Private native profile → atomic conversation/settings files
  ├── Versioned WASM command host → bounded text transformations
  └── Rust MCP bridge → optional external tools / Node compatibility host
```

## Ownership

- `crates/app` owns native controls, selection, visible streaming/error states,
  request cancellation, and the in-memory API key. Network work runs outside the
  GTK event loop.
- `crates/core` owns provider requests, streamed response parsing, cancellation,
  session state, and private persistence. It does not serialize credentials or
  return remote response bodies in error messages.
- `crates/plugins` owns a versioned manifest, local installation/removal, plugin
  validation, and the bounded WebAssembly execution environment. Plugins do not
  run inside GTK or receive arbitrary host services.
- `crates/mcp` owns the standard stdio MCP client, scoped tool discovery,
  bounded protocol/results, explicit environment forwarding, cancellation and
  owned subprocess cleanup. SDK tracing remains disabled for private payloads.
- `compat` reuses the pinned upstream Cordis runtime in an optional Node host.
  It exposes only the selected plugin's tools, with default upstream tools
  disabled. It does not reinterpret the upstream service API in Rust.

Persistent state belongs exclusively to the native client. Its profile is
`$XDG_DATA_HOME/deepseek-harness-native`, with the normal fallback beneath
`~/.local/share`. It does not read or write
`local.deepseek.harness-linux/harness`, `~/.dsh`, or upstream session formats.
Any future import must work on copied fixtures and preserve the original data.
Two engines must not write the same conversation store.

## Provider contract

The backend sends OpenAI-compatible Chat Completions requests. HTTPS is required
for remote endpoints; loopback HTTP is accepted for isolated local testing.
Redirects are disabled. Errors use fixed user-facing messages, and the key is
passed only to the active request in memory. Model availability and billing
remain the provider's responsibility.

The interface batches visible updates rather than treating each network chunk
as a new widget tree. The store checkpoints replies and records completion,
cancellation, failure, or interruption. A restart recovers an unfinished reply
as interrupted rather than continuing to show it as active.

## Plugin contract

Version 1 is deliberately narrow: a plugin is an installable local directory
containing a manifest and WebAssembly module. It contributes a named text
command. Input is composer text; output is replacement composer text.

The host uses Wasmi and grants no WASI, filesystem, network, process, or secret
imports. Version 1 limits a module to 4 MiB, text input/output to 64 KiB, guest
linear memory to 8 MiB, and execution to 2 million fuel units. The memory limit
applies to guest linear memory, not the application's total process memory.
The manifest/API version is checked before a plugin becomes available.
This API can evolve behind explicit versions instead of freezing the current
Harness internals.

Installation accepts regular local files, rejects symlinks and special files,
and registers a validated package atomically. An existing plugin ID must be
removed explicitly before replacement; installation does not silently overwrite
an installed module.

The WASM ABI remains unchanged. Installed commands are also projected into the
model's frozen tool catalog with a required text argument and explicit approval.
MCP tools use a server-scoped identity and a frozen name-to-executor mapping for
the whole turn. Server instructions are not promoted into system instructions.

The core handles streamed tool-call assembly and a bounded eight-round loop.
It checkpoints the complete plan before dispatch, waits for approval/execution,
then checkpoints each outcome before another call or model request. Failed
checkpoints stop progression. Tool traces include reasoning and canonical
assistant/tool messages beneath the visible reply. Stop/restart preserves
uncertain outcomes as errors and never automatically replays a saved call.

Trace-bearing profiles use format 2 so an older client cannot silently discard
tool history. Actual DeepSeek reasoning is preserved; older DeepSeek history
without that reasoning requires a fresh conversation when enabling tools.

JavaScript compatibility covers exported server-side tools from selected
modules and plugin bundles, using a separate upstream Agent/Session. React
views and plugins that change the upstream agent loop do not become native UI
or native agent hooks. No original Harness profile is imported or modified.
The Node compatibility host and arbitrary MCP tools have user-account authority;
they are not constrained by the WASM text extension's resource sandbox.

## Validation boundary

Core tests should cover streamed frames, UTF-8 boundaries, cancellation,
credential-safe errors, atomic persistence, and interrupted-state recovery.
Plugin tests should cover installation validation, path/symlink rejection,
unsupported imports, and execution/resource limits. Desktop smoke checks should
exercise the actual GTK controls with a disposable profile and local provider.

Ubuntu CI builds the release installer on 24.04. Any installed-package test on
26.04 must use that same file and checksum. Xvfb proves native widget behavior
in a headless X11 session; physical Wayland/GPU compatibility and live provider
authentication require separate checks. Test results belong in the change's
validation report; this document does not imply those checks have already run.
