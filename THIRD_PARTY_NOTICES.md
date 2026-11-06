# Third-party software

This repository contains the Rust/Tauri desktop shell and its startup interface.
It uses the original [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
runtime through the pinned npm dependency `@deepseek-ai/dsh@0.2.1-rc.5`.
The upstream runtime is MIT licensed, copyright (c) 2026 DeepSeek. Its license
text is included in [licenses/deepseek-harness-MIT.txt](licenses/deepseek-harness-MIT.txt).

This is an independent, unofficial project. DeepSeek's names and branding identify
the upstream project; this shell is not an official DeepSeek desktop release.

The lockfiles record the Rust and npm dependencies used by the shell and runtime.
Each dependency retains its own license. Dependencies, the Node executable, and
built installers are not committed to this repository. Build preparation copies
the official Node.js distribution's complete `LICENSE` file, including its
third-party notices, into the bundled runtime as `runtime/NODE-LICENSE`.
Installers also include this notice, the shell's MIT license, and the upstream
Harness license under the application's `licenses/` resources directory.
The installed npm packages retain their accompanying license files.

The runtime also includes [pnpm](https://github.com/pnpm/pnpm), pinned to
`11.28.11`, for upstream Harness's plugin-management operations. pnpm is MIT
licensed; its accompanying `LICENSE` remains under `runtime/node_modules/pnpm/`.
The package manager runs with the bundled
Node executable and does not require a system pnpm installation.
