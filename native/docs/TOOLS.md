# Native tool connections

The native app supports three sources in one per-conversation-turn catalog:
installed WASM text extensions, local stdio MCP servers, and server-side
JavaScript Harness tools through the optional compatibility host.

WASM commands remain manual composer actions by default. Enable **Offer WASM
extensions to the model** in **Tool connections** to add them to the model's
catalog. Each model invocation still requires approval.

## Local MCP server

In **Tool connections → Add MCP server**, enter:

- **Connection ID:** a unique name using letters, digits, `_` or `-` (24 maximum).
- **Program:** an installed executable, preferably its absolute path.
- **Arguments:** one literal argument per line; there is no shell evaluation.
- **Working folder:** optional absolute existing folder.
- **Environment variable names:** optional comma-separated names. Values are
  obtained from the app's environment at connection time, never saved in the
  connection file. The model API key is never forwarded.

Saving records the private launch settings. **Connect** starts the process and
discovers its tools. **Disconnect** stops it. A timeout or cancellation closes
that connection, so reconnect before the next use. Catalogs refresh on reconnect.
There is no automatic reconnection or process startup after restarting the app.

For Playwright MCP with your existing signed-in Chrome tabs, install its
[official browser extension](https://playwright.dev/mcp/configuration/browser-extension)
and configure an installed `npx` executable with these argument lines:

```text
-y
@playwright/mcp@latest
--extension
```

Node/npx must already be installed. Use a reviewed pinned server version for a
repeatable setup. Browser setup and connection approval belong to the browser;
the native app does not automatically read profiles, tabs or cookies.

## Existing JavaScript tools

Choose **Add JavaScript tool plugin**, supply the installed upstream runtime,
an absolute plugin file/package-folder path, and optionally a settings file.
The supported runtime is exactly `@deepseek-ai/dsh@0.2.1-rc.5`. An existing
Tauri installation normally supplies it at
`/usr/lib/DeepSeek Harness for Linux/runtime`. A prepared source `runtime/`
folder also works with a separately installed Node executable.

A harmless first check is the existing plugin folder
`/usr/lib/DeepSeek Harness for Linux/runtime/node_modules/@deepseek-ai/dsh-tool-working-directory`.
After connecting, ask the model for the working directory and approve the call.
The host inherits the app's working folder; use the general MCP connection form
with an explicit Working folder if a tool requires another project directory.

The app ships both compatibility host modules beneath
`/usr/share/deepseek-harness-native/compat/`. It launches the runtime's Node
executable when present, otherwise an installed Node found at configuration time.
The host creates its own profile under the native profile's `compat/<id>/`.
Never point that host at the original Harness profile.

The host runs unchanged Cordis modules or plugin bundles and exports their tool
schemas and execution over MCP. Default upstream tool plugins, hot reload and
the package manager are disabled. Plugins remain trusted JavaScript code with
the user's filesystem/network/process authority. React-only or agent-hook-only
plugins do not acquire a native equivalent through this bridge. Plugins that
query upstream sessions see the isolated compatibility session, not native chats.

## Approval, history and limits

Each model-requested call pauses at **Allow tool call?**. Review its name and
arguments; choose **Allow once**, **Deny**, or **Stop task**. Calls and outcomes
are visible in **Tool activity** and saved in the private conversation store.
Tool output is untrusted conversation content and may be sent to the model
provider. It does not become system instructions.

The plan must save before execution and each result must save before continuing.
An interrupted call can have an unknown external outcome; inspect the external
application before retrying. Reopening a conversation never re-executes a call.
Existing DeepSeek chats without saved reasoning need a fresh chat for tools.

Limits: 16 configured connections, 64 advertised tools, eight tool rounds and
eight calls per round, 64 KiB arguments/results, bounded protocol frames and
timeouts. MCP results are text/structured data; media, remote HTTP/OAuth,
sampling, elicitation and background MCP tasks are not supported in this version.
