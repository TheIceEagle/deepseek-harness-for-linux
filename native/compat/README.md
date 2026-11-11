# Optional JavaScript tool-plugin compatibility

This host runs an existing server-side Harness plugin in the unchanged Cordis
runtime and exports its registered tools through local MCP stdio. The native
Rust app remains the model client and owns approval and tool-call history.
Connecting this host starts Node and the pinned upstream services for that
connection's lifetime. It is optional and is never started by ordinary native
chat or a WASM text extension.

## Runtime and plugin selection

Use the existing Harness runtime directory containing
`node_modules/@deepseek-ai/dsh/package.json`. Its version must be exactly
**0.2.1-rc.5**. An installed original desktop app normally has this runtime under
`/usr/lib/DeepSeek Harness for Linux/runtime`. For a source checkout, prepare it
with the repository's tested Node 26.10.0 / npm 12.1.0 toolchain:

```sh
npm ci --prefix runtime
```

The host loads an already-installed JavaScript module or package folder. Its own
ordinary dependencies must already be installed; the upstream profile resolver
supplies the pinned Harness peer packages. It accepts both Cordis plugin modules
(function/namespace or default class exports) and packages declaring
`dsh.bundle.patch`. Bundle composition uses upstream Loader patch semantics.
No npm package or install script is fetched or executed when connecting.

Configure this as a local stdio MCP server in the native app. The server command
is Node with these arguments; it is not an interactive terminal chat command:

```sh
node /absolute/path/native/compat/host.mjs \
  --runtime /absolute/path/runtime \
  --profile /absolute/path/native-profile/js-compat/example \
  --plugin /absolute/path/to/installed-plugin
```

The packaged host lives at
`/usr/share/deepseek-harness-native/compat/host.mjs`; `runtime.mjs` must remain beside
it. The native application's JavaScript connection form supplies these paths.
Choose a new, empty compatibility profile below the native app's data folder.
The host marks and locks its own profile and refuses an unrelated nonempty
folder. It never opens the original Harness profile or imports the native chat
store. Each connection owns one separate upstream Agent/Session, initially idle;
creating it does not make an LLM request.

For an actual upstream tool already present in that runtime, use:

```text
--plugin @deepseek-ai/dsh-tool-working-directory
```

This exports only `working_directory`, running through the real upstream Agent,
Session, services and tool-execution pipeline. The unchanged
`@deepseek-ai/dsh-experimental-session-search` bundle is also covered by tests.
Its session-search tools inspect the compatibility host's own session data.

## Configuration and limits

`--plugin-config-file /absolute/private/config.json` is optional. For an ordinary
plugin it contains the plugin's JSON configuration object; for a bundle it
contains a JSON Cordis patch array applied after the bundle. It is limited to
64 KiB. Supply configuration through this file instead of putting credentials
into process arguments. Do not edit the generated compatibility profile files.

The default upstream tool consumers are disabled. Only tools registered by the
selected plugin or bundle are advertised and callable. Profile hot reload and
package-manager operations are disabled; reconnect after changing the selection.
The MCP surface provides initialization, ping, tool discovery/execution and
cooperative cancellation. It has no sampling, elicitation, resource or prompt
provider. The protocol baseline is 2025-06-18. Limits are 64 tools, 1 MiB frames,
64 KiB rendered results, four pending calls and a 60-second tool timeout. The
Rust caller can impose an earlier deadline or stop the process.

By default, ambient credential-named variables and inherited `DSH_*` configuration
are removed before boot. A plugin may receive an explicitly approved variable
using repeatable `--env-name GITHUB_TOKEN` arguments. Only names appear in these
arguments; the values are inherited from the approved launch environment. The
native app supplies these flags for the variable names approved in its MCP
connection settings. `DEEPSEEK_API_KEY` and `DSH_*` names are always forbidden,
including case variants; requesting them rejects startup before creating a
profile. Provider credentials held by the Rust app are not supplied to this
host. Ordinary upstream stdout/stderr logging is discarded; stdout is
reserved for MCP frames, and startup failures use a fixed message. Upstream
permission checks, schemas, cancellation and content policies still apply.

## Compatibility boundary

JavaScript plugins run as trusted Node code with the process user's operating
system access. This is the existing upstream execution model; WASM's no-import
sandbox does not apply. The private profile separates data and configuration,
not operating-system authority. Install and select plugins you trust.

The bridge supports **server-side tool contributions**. React plugin panels,
WebView slots, browser UI, native settings forms for arbitrary plugin schemas,
and direct modifications to the Rust agent loop are outside this contract.
Plugins requiring missing client/Host services can fail to activate; a selection
registering no usable server tools fails instead of appearing connected.
Upstream session/agent hooks affect the compatibility Session. They do not
receive or control the Rust conversation, model requests, subagents or history
automatically. A bundle that contributes only UI or agent-loop effects does not
become a native UI extension through this bridge.

## Validation

```sh
python3 native/compat/tests/test_compat.py
```

These checks launch actual Node/Cordis processes with disposable profiles and
workspace directories. They cover an unchanged upstream tool plugin and bundle,
normal plugin peer imports, approved credential forwarding with model-key
isolation and log containment, cancellation,
result/frame limits, profile locks, crash reopening, unrelated-profile protection,
unsupported runtime rejection, and zero-tool failure. They use no browser, real
provider key or LLM call. Set `HARNESS_COMPAT_RUNTIME` to test another copy of the
same pinned runtime. The standard native checks do not require Node; this suite
is specifically for the optional compatibility path.
