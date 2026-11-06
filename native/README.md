# DeepSeek Harness for Linux — native preview

An unofficial native Linux client written in Rust, with a GTK4 interface and a
Rust chat backend. It does not start Node, a browser engine, or the upstream
Harness server. It is not affiliated with or endorsed by DeepSeek.

This is a separate application under development. The existing Tauri application
remains available in the repository root. The native client does not import,
modify, or share its Harness profile.

## Build and run

Tested platforms are Ubuntu **24.04.5 LTS** (GTK 4.14.5) and **26.04.1 LTS**
(GTK 4.22.4), x86_64, using the same installer built on 24.04. Use Rust **1.93.1** with
the `rustfmt` and `clippy` components. Install the native development libraries:

```sh
sudo apt install build-essential pkg-config libgtk-4-dev
rustup toolchain install 1.93.1 --component rustfmt --component clippy
```

From the repository root:

```sh
cargo +1.93.1 run --manifest-path native/Cargo.toml -p harness-native-app
```

Use the app's settings to select an OpenAI-compatible endpoint and model, then
enter an API key for the current process. Alternatively, start the app with
`DEEPSEEK_API_KEY` already present in its environment. Do not put a key in a
command-line argument or commit it to a configuration file. Credentials are not
saved with conversations or settings.

Closing the native application ends its active request. A stopped or interrupted
reply remains in the saved conversation with its status. The original Harness
application's hide-to-tray behavior does not apply to this preview.

## Data and plugins

The native profile is stored at:

```text
$XDG_DATA_HOME/deepseek-harness-native/
```

With the usual Ubuntu defaults, this is
`~/.local/share/deepseek-harness-native/`. Conversation and settings writes are
atomic, and a profile lock prevents two writers from changing the same store.
Settings include the endpoint, model, and system prompt, but no API key.

The first plugin API accepts local WebAssembly text commands. Plugins receive
the composer text and return replacement text; the application applies the
result only after a successful run. They receive no filesystem, network, shell,
or credential access. The host limits module size, input/output, memory, and
execution fuel.

To build the harmless Uppercase example from source:

```sh
cargo +1.93.1 run --manifest-path native/Cargo.toml --locked -p harness-native-plugins --example build-plugin
```

In **Extensions**, install the `native/examples/uppercase` folder. The Debian
package includes the same example at
`/usr/share/deepseek-harness-native/examples/uppercase`. It changes ASCII
letters in the composer to uppercase while preserving other UTF-8 characters.

This is a new plugin contract. Existing npm/Cordis plugins, React plugin views,
MCP servers, and the upstream Harness agent/tool loop are not implemented by
this preview. See [architecture](docs/ARCHITECTURE.md) for the boundary.

## Checks

```sh
cargo +1.93.1 fmt --manifest-path native/Cargo.toml --all -- --check
cargo +1.93.1 clippy --manifest-path native/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo +1.93.1 test --manifest-path native/Cargo.toml --workspace --locked
cargo +1.93.1 build --manifest-path native/Cargo.toml -p harness-native-app --locked
```

Backend tests use a local simulated OpenAI-compatible service. They do not
require a key or incur provider charges. Native desktop checks require Xvfb, a
session bus, GTK4, and local fonts. CI checks do not establish physical Wayland
or GPU compatibility on every desktop.

To run the GTK workflow test after building the example:

```sh
sudo apt install xvfb dbus-x11 fonts-ubuntu python3-gi gir1.2-gtk-4.0 python3-pyatspi at-spi2-core python3-pil xdotool tesseract-ocr tesseract-ocr-eng
python3 native/tests/smoke-native.py native/target/debug/deepseek-harness-native
```

The script creates a private Xvfb display, session bus, and disposable profile.
It checks real native controls against a local simulated provider, including
streaming and cancellation, retry and failed-save recovery, local extensions,
restart persistence, one-instance activation, and bounded history paging.
Local screenshot recognition verifies that response markers are visible above
the composer, including after a long code block and in a narrow window.

## Debian package

Build on Ubuntu **24.04** to target the older supported system libraries:

```sh
sudo apt install dpkg-dev
bash native/scripts/build-native-deb.sh
```

The script creates `native/dist/deepseek-harness-native_0.2.0~alpha1_amd64.deb`
and `SHA256SUMS`. It derives runtime library dependencies from the compiled
binary. Verify and install the package:

```sh
cd native/dist
sha256sum --check SHA256SUMS
sudo apt install './deepseek-harness-native_0.2.0~alpha1_amd64.deb'
```

The separate package and desktop entry are named **DeepSeek Harness for Linux
(Native Preview)**. Installing it does not replace the Tauri package.

## Preview measurements

Measured on 2026-11-06 with the same **0.2.0-alpha.1 release** installer built
with Rust 1.93.1 on Ubuntu 24.04.5. Each run used one native app process, a private
Xvfb display with software rendering, and five synthetic loopback requests.

| Installed OS | Ready process-tree PSS | After chat/extension workflow |
| --- | ---: | ---: |
| Ubuntu 24.04.5, GTK 4.14.5 | 91.7 MiB | 102.4 MiB |
| Ubuntu 26.04.1, GTK 4.22.4 | 92.8 MiB | 105.5 MiB |

These are point-in-time samples, excluding the test display and fixture server.
Startup latency, peak memory and live provider latency were not measured. A
speedup over the original app requires the same workload and measurement method.

See [validation methods](docs/TESTING.md) for the isolated desktop workflow and
the boundary of the performance reports.
