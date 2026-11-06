# DeepSeek Harness for Linux

**Unofficial community project. Not affiliated with or endorsed by DeepSeek.**

An Ubuntu desktop shell for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), using Rust and Tauri. It starts the original Harness Web backend and displays its original interface in a desktop window. The agent, models, tools, settings, and conversation UI stay upstream.

A separate [native Rust client preview](native/README.md) is being developed in
`native/`: GTK4 interface, Rust streaming backend, private conversation storage,
and installable WASM text extensions. It uses no Node or WebView. This is an
independent implementation with its own profile and plugin contract, rather than
full compatibility with the upstream Harness agent and JavaScript plugins.

The npm runtime is pinned to **`@deepseek-ai/dsh@0.2.1-rc.5`**. This project is independent of DeepSeek's official desktop distribution. Four transitive dependencies are overridden to patched versions: `undici@8.12.2`, `zod@4.6.7`, `fflate@0.8.3`, and `@modelcontextprotocol/client@2.3.0`.

The desktop shell is [MIT licensed](LICENSE). See
[third-party notices](THIRD_PARTY_NOTICES.md) for upstream attribution.
The [override notes](runtime/DEPENDENCIES.md) explain the dependency pins and when
they can be removed.

## Install

Verified `.deb` packages and `SHA256SUMS` are published through
[GitHub Releases](https://github.com/TheIceEagle/deepseek-harness-for-linux/releases)
after the tagged build and installed-package checks pass. Until the first binary
release is published, use the source build below.

Download the installer and its checksum file into the same folder, verify the
download, then install it. Adjust the version in the filename to match the release:

```sh
sha256sum --check SHA256SUMS
sudo apt install './DeepSeek Harness for Linux_0.1.2_amd64.deb'
```

Open **DeepSeek Harness for Linux** from the applications menu. Installed packages
include Node and the upstream runtime; no separate Node installation is needed.
Updates use a newly downloaded package; the app does not update itself.

## Build

Target: **updated Ubuntu 24.04 LTS and 26.04, Linux x86_64**. CI builds the
installer on 24.04 and checks the same package on both versions, avoiding a
dependency on the newer distribution's build libraries.

The tested build toolchain is **Node.js 26.10.0, npm 12.1.0, and Rust 1.93.1**
with the `rustfmt` and `clippy` components. Install Rust using
[rustup](https://rustup.rs/); Ubuntu 24.04's default Rust packages are older than
this toolchain. Use an official Node.js distribution that includes its `LICENSE`
file: preparation bundles the executable and its complete third-party notices.
npm 12.1.0 enforces the runtime's existing native install-script allowlist.

Install the Ubuntu build libraries:

```sh
sudo apt install build-essential pkg-config \
  libwebkit2gtk-4.1-dev libssl-dev libxdo-dev \
  libayatana-appindicator3-dev librsvg2-dev curl wget file
```

See the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for other distributions.

Both supported Ubuntu versions provide WebKitGTK's **4.1 API**. Current validation
uses WebKitGTK **2.52.6**; older WebKit versions are untested. Native GUI checks run
under Xvfb with a fresh profile and no model request. They do not establish GPU
or Wayland compatibility for every desktop environment.

| OS | Current verification |
| --- | --- |
| Ubuntu 24.04.5 LTS x86_64 | Clean source build, 12 Rust tests, backend and all native recovery/tray checks; isolated Docker/Xvfb environment |
| Ubuntu 26.04.1 x86_64 | Local source build, 12 Rust tests, backend and all native recovery/tray checks; Xvfb environment |

CI also installs the same 24.04-built `.deb` on both systems and repeats the
native recovery checks. Each job records its tool and WebKit versions in its
summary and retains its reports. No profile data migrations are required.

Get the source and enter the project directory:

```sh
git clone https://github.com/TheIceEagle/deepseek-harness-for-linux.git
cd deepseek-harness-for-linux
```

If using rustup, select the tested toolchain inside the project directory:

```sh
rustup toolchain install 1.93.1 --component rustfmt --component clippy
rustup override set 1.93.1
```

Install the dependencies and start the development app:

```sh
npm ci
npm ci --prefix runtime
npm run dev
```

To create the Ubuntu installer:

```sh
npm run build
```

The `.deb` is written under `src-tauri/target/release/bundle/deb/`; the current installer is approximately **176 MiB**. Build preparation copies the current Node executable into the runtime; use a Node installation suitable for the target machine. The package includes Node and the installed Harness dependencies, so an installed application does not need a separate Node installation.

Install the generated package with Ubuntu's package installer, then open **DeepSeek Harness for Linux** from the applications menu.

The renamed package replaces the earlier **Harness for Linux** installation. Its
application identifier and saved-profile location remain the same.

## Using the app

The launcher starts Harness on a dynamically assigned loopback port and waits for its authenticated startup URL before opening the workspace. The service is bound to `127.0.0.1`.

Enter your API key in the **original Harness interface**, under **Settings → Models**, when prompted. Do not paste it into this development chat. Model access and usage charges follow your provider account.

Closing the window hides the application and keeps its backend running. Reopen it from the tray; choose **Quit DeepSeek Harness** to stop the backend and exit. Quitting interrupts active work.

The tray's read-only status shows **Workspace running** or **Workspace stopped**.
Running refers to the local service; it can be idle. If it stops unexpectedly,
open the app again or use **Try again** on the recovery screen.

Reopening a healthy workspace preserves its current page and unsent draft. If the view is blank or stuck loading, the shell reopens it using the backend's private launch address without stopping a healthy backend. If the backend has stopped, reopening retries startup. The tray's **Reload workspace** action provides a manual way to reload the view; it may discard an unsent draft. An interface that cannot finish startup within 45 seconds returns to the app's retry screen.

On first use, upstream Harness creates `<Documents>/deepseek-harness/default-workspace/`. On Ubuntu, `xdg-user-dir DOCUMENTS` selects the Documents directory; the application's working directory does not control this location. If the Documents directory is unavailable, use **Choose workspace** to select a folder.

Harness uses a separate profile beneath Tauri's application data directory:

```text
$XDG_DATA_HOME/local.deepseek.harness-linux/harness/
```

With the usual Ubuntu defaults, this is `~/.local/share/local.deepseek.harness-linux/harness/`. Settings, credentials, and sessions live there, separate from a normal `~/.dsh` installation.

## Performance and compatibility

Tauri uses the system WebKitGTK engine instead of shipping Electron's Chromium runtime. A smaller desktop shell and lower shell memory use are expected, but no percentage improvement has been measured. Node and the Harness backend still run; the package retains their dependencies.

Cloud model generation is unchanged. A Rust window does not make DeepSeek's servers generate answers faster. Startup time, memory use, and interface responsiveness need measurements on the same workload before making comparisons.

| Measurement | Recorded result (0.1.0) |
| --- | --- |
| Isolated backend startup to readiness | 1,026 ms |
| Isolated backend graceful shutdown | 72 ms |
| Installed desktop visible interface | 4,831 ms |
| Installed desktop idle memory (PSS, seven processes) | 611 MiB |
| Normal Ubuntu desktop document load | 4,952 ms |
| Normal desktop current total memory (PSS, seven processes) | 809 MiB |
| Comparison with Electron | Not measured |

The backend timings are one smoke-test run. Desktop measurements use the installed
release on Ubuntu 26.04 under Xvfb, with a fresh profile and no API request. The
desktop figure includes Node and WebKit; it is not the memory of the Rust shell
alone. Xvfb rendering and first-use setup affect these results. These are absolute
measurements, not evidence of an improvement over Electron.

The normal desktop figures were sampled from the running installed app, with its
current UI state. They use the real desktop renderer rather than Xvfb; memory
varies with open views and tasks.

These measurements predate the reopen recovery changes. New timing records
distinguish `workspace_document_loaded` from `workspace_ready`: a loaded document
can still be waiting for the interface to mount. The 4,952 ms figure measures
document load, not workspace readiness.

This application provides the **Web profile's capabilities**. Features implemented by the official Electron shell need their own Linux integration. In particular:

- The Web sidebar browser is disabled by default and uses an iframe when enabled. Some sites refuse embedding; it does not inherit Electron's embedded browser behavior.
- Electron's native file dragging, embedded account pages, and desktop-specific integrations do not automatically carry over.
- This shell has no official DeepSeek desktop updater. Runtime upgrades require an explicit dependency update, verification, and a new build.
- Model requests and plugin installation are not exercised by the current smoke tests. The package includes Node but does not bundle pnpm.

References: [Harness Web flags, authentication, and readiness](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/web-app/README.md), [default workspace policy](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/api/workspace-controller/README.md#first-use-workspace), [Web and Electron browser differences](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/client/ui-sidebar-browser/README.md), [official desktop architecture](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/desktop/README.md). Upstream's current documentation may describe changes newer than the pinned runtime.

## Validation

The **0.1.2 release-readiness changes** pass formatting, strict Clippy, and all
**12 Rust tests**. Native tests additionally verify actual tray status across
startup failure, normal startup, backend exit, restart, and renderer recovery.
Existing draft preservation and all six reopening/recovery scenarios remain
covered. Backend checks verify the actual owned HTTP socket binds only to
`127.0.0.1`, and startup credentials are absent from desktop output, logs, and
process arguments. The bundled startup page has no unused `core:default` grants;
the hosted workspace remains unable to invoke desktop startup commands.

Normal push/PR CI tests the pinned runtime on both supported Ubuntu versions.
Tagged release builds must pass that CI, the package version/tag consistency
check, and installation/recovery checks before the existing tested artifact is
published with `SHA256SUMS`. There is no automatic merge or upstream-version
activation.

The **0.1.1 recovery changes** have passed the following development checks:

- `cargo test --manifest-path src-tauri/Cargo.toml --jobs 2`: **9 passed**.
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`: passed.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --jobs 2 -- -D warnings`: passed.
- Strict static UI audit: **0 findings**.
- Before-fix regression reproduced: a blank test view with a healthy backend remained blank after a second application launch, reaching the test's 15-second timeout.
- `HARNESS_TEST_REOPEN=1 HARNESS_TEST_WEBKIT_CRASH=1 node scripts/smoke-desktop.mjs`: passed for a healthy reopen preserving the draft, stale loading, a blank renderer, a stopped backend, reopening immediately after backend exit, and a terminated WebKit renderer. Renderer recovery preserved the healthy backend. The crash check verified a replacement renderer and workspace readiness; its native Xvfb screenshot was also visually checked because WebDriver retains its old page context after a renderer crash.

The renamed **0.1.1 release** also passed installation and recovery checks:

- `CARGO_BUILD_JOBS=2 npm run build`: produced `DeepSeek Harness for Linux_0.1.1_amd64.deb` (175.86 MiB after final packaging).
- `npm run tauri -- bundle --bundles deb`: packaged the rename's replacement/conflict metadata for the earlier `harness-for-linux` package.
- `sudo -n dpkg -i 'src-tauri/target/release/bundle/deb/DeepSeek Harness for Linux_0.1.1_amd64.deb'`: installed successfully and replaced 0.1.0. The old launcher entry was removed; the new entry is named **DeepSeek Harness for Linux**. `desktop-file-validate` passed.
- `HARNESS_TEST_APPLICATION=/usr/bin/harness-for-linux HARNESS_TEST_REOPEN=1 HARNESS_TEST_WEBKIT_CRASH=1 node scripts/smoke-desktop.mjs`: all six recovery cases passed against the installed release, including privilege isolation and a visually verified native screenshot after renderer recovery.
- Launching the installed desktop entry on the normal Ubuntu desktop reached `workspace_ready` in **5,059 ms**, using the existing profile. This is a single launch measurement, with no model request made by the verification script.

The performance table above and these completed runtime/installed-app checks
describe the preceding **0.1.0 release**, before the recovery changes:

- `npm audit` from `runtime`: **0 known vulnerabilities** after the dependency overrides.
- Backend HTTP smoke: authenticated workspace HTML **200**, application JavaScript **200**, graceful exit **0**; timings appear above.
- Native WebKit checks: original Harness interface rendered, and backend crash recovery with retry passed. The production shell performs one bounded same-origin reload during startup, preserving upstream's `HttpOnly; SameSite=Strict` authentication cookie.
- `HARNESS_TEST_FAILURE=1 node scripts/smoke-desktop.mjs`: missing-runtime error, keyboard retry, 640px window, and scrollbar checks passed.
- `HARNESS_TEST_APPLICATION=/usr/bin/harness-for-linux HARNESS_TEST_CRASH=1 node scripts/smoke-desktop.mjs`: installed interface, privilege isolation and crash/retry passed. A subsequent installed-app run produced the timing and memory figures above.
- `npm run build` produced the Ubuntu installer; `sudo dpkg -i` installed it successfully. `npm run tauri -- bundle --bundles deb` refreshed the final dependency metadata.
- Static UI audit: no findings. DESIGN.md lint: no errors (seven informational orphan-token warnings because CSS is canonical).

Model requests require an API key and were not tested. No comparison with Electron
or cloud generation speedup has been measured.

The smoke scripts and their reports are in `scripts/` and `artifacts/`. Desktop
tests need `xvfb`, `webkitgtk-webdriver` and `cargo install tauri-driver --locked`.
The optional renderer-crash screenshot also needs Python 3 with PyGObject and Gdk
3 bindings (`python3-gi` and `gir1.2-gtk-3.0` on Ubuntu).
They run on a separate session bus and restrict cleanup to their own process tree,
so an app already open on the user's desktop is unaffected.
