# Uppercase text plugin

A harmless example for native text-plugin API version 1. It changes ASCII
letters to uppercase and preserves all other UTF-8 bytes. It cannot access files,
the network, environment variables, credentials, or other conversations.

Generate `plugin.wasm` from its readable source:

```sh
cargo run --manifest-path native/Cargo.toml -p harness-native-plugins --example build-plugin
```

In the native application's **Extensions** window, choose **Install extension**, select
this `uppercase` folder, then apply **Make uppercase** to a draft. Installation
copies only `plugin.json` and `plugin.wasm` into the separate native profile.
It does not modify the source folder. Removing the installed plugin leaves the
source example available for another installation.

The native plugin API is independent of the original Harness/Cordis ecosystem.
Existing JavaScript, npm, and React plugins are not compatible with this API.
