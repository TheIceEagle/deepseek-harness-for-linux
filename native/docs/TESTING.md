# Native preview validation

Run from the repository root with Rust 1.93.1:

```sh
cargo fmt --manifest-path native/Cargo.toml --all -- --check
cargo clippy --manifest-path native/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path native/Cargo.toml --workspace --locked
cargo build --manifest-path native/Cargo.toml --locked
cargo run --manifest-path native/Cargo.toml --locked -p harness-native-plugins --example build-plugin
python3 native/tests/smoke-native.py native/target/debug/deepseek-harness-native
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
input methods or GPU drivers. The preview implements chat and explicit text
extensions; it does not implement model-directed tool calls or existing
JavaScript plugins.
