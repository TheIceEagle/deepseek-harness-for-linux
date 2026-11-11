# Native preview validation

Run from the repository root with Rust 1.93.1:

```sh
cargo fmt --manifest-path native/Cargo.toml --all -- --check
cargo clippy --manifest-path native/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path native/Cargo.toml --workspace --locked
cargo build --manifest-path native/Cargo.toml --locked
cargo run --manifest-path native/Cargo.toml --locked -p harness-native-plugins --example build-plugin
python3 native/tests/smoke-native.py native/target/debug/deepseek-harness-native
python3 native/tests/smoke-tools.py native/target/debug/deepseek-harness-native
```

The Rust tests cover streaming frame boundaries, Unicode, cancellation under
backpressure, credential-safe failures, atomic persistence and recovery,
bounded history pages, native Markdown attributes, and extension validation and
resource limits. They never need a real provider key.

The desktop test drives actual GTK controls through AT-SPI and X11 input. A
synthetic loopback provider controls streaming, failures, and cancellation. A
temporary profile, private session bus and cleared API-key environment isolate
the run from the user's app and conversations. Local screenshot OCR confirms
that a final marker after a long code block is drawn inside the history viewport;
saved text alone is insufficient. Screenshots and a JSON report are written to
the ignored `artifacts/` directory.

The workflow also exercises drafts, retries, failed-save recovery, close and
restart, a local WASM extension, and narrow windows. Memory reports sum PSS from
`/proc` across the owned application process tree after readiness and after the
workflow. Xvfb, the fixture server and the automation process are excluded. This
software-rendered workload is not a comparison against the original Tauri app.

CI builds the Debian installer on Ubuntu 24.04, verifies its SHA256SUMS, and runs
the installed workflow on both Ubuntu 24.04 and 26.04 using the same file. The
installer has a separate package, desktop entry and profile. No original Harness
profile is read or migrated during testing.

These checks do not validate a real provider account, billing, physical Wayland
input methods or GPU drivers. The tool bridge implements local stdio MCP and
server-side JavaScript tools. React panels, upstream agent-loop plugin hooks,
remote HTTP/OAuth and media tool results remain outside its current contract.

## Tool bridge checks

The tool desktop workflow drives the actual connection form and approval modal
against a disposable Python MCP child and synthetic model provider. It checks
that saving does not spawn, Deny does not execute, Allow once executes exactly
once and returns its result to the model, failed plan saves block dispatch,
Retry save does not repeat work, Stop/disconnect clean up children, and restart
keeps servers stopped. It uses six local model requests and two controlled tool
executions. It does not attach to a real Chrome session.

The optional compatibility host has a separate test suite:

```sh
python3 native/compat/tests/test_compat.py
cargo run --manifest-path native/Cargo.toml --locked -p harness-native-mcp --example check_compat -- "$PWD/runtime"
```

These checks require Node and the installed pinned upstream `runtime/` packages.
They execute unchanged upstream tool plugins and a real plugin bundle through
Cordis, plus the official Rust MCP client against that host. They use isolated
profiles and synthetic credentials. Ordinary native build/tests do not require
Node. Native CI runs the compatibility job separately.

The MCP tests exercise actual child processes, bounded discovery and results,
environment isolation, timeout/cancellation, normal descendant cleanup,
concurrent close/reaping and direct-child shutdown after parent death. Arbitrary
external MCP programs and trusted JavaScript plugins are not a process sandbox.

Lifecycle fixtures also cover discovery-only modern servers, modern per-request
metadata, same-process legacy fallback, a silent legacy probe with a short
deadline, responsive slow initialization/catalog loading, modern rejection and
malformed-response handling, and cancellation around startup fallback. Both
lifecycles retain the same tool approvals, bounds and process cleanup.

Format-1 conversation fixtures remain readable. Saved reasoning/tool traces
upgrade to format 2, which earlier alpha clients refuse. Recovery records an
unknown outcome and never replays a tool. DeepSeek conversations predating saved
reasoning require a new chat before enabling tools; this guard runs before HTTP.

## Earlier preview baseline

On 2026-11-06, native **0.2.0-alpha.1** passed formatting, strict all-target
Clippy, 38 Rust tests and the complete desktop workflow. The same release
installer passed the installed workflow on Ubuntu 24.04.5 / GTK 4.14.5 and
Ubuntu 26.04.1 / GTK 4.22.4. Each desktop run made five local fixture requests.
The original Tauri application's formatting, strict all-target Clippy and
12 Rust tests also passed against the main baseline.

The locally tested installer SHA256 is
`bb9bc2aa6bb7da539f822cd7fc0c00742dd1e428edd91bef6e964a7feed2db52`;
its installed binary SHA256 on both systems is
`479cee6faab7aee03632a24c28bf1c47378e66b001c84260c184c385bb6c5800`.
CI builds its own installer and records its own checksum; package timestamps can
produce different archive hashes from the same source. The
[README measurements](../README.md#preview-measurements) describe this local
release build and workload.
