# Native MCP transport

This Rust crate connects an explicitly approved local executable through MCP
stdio. It uses the official `rmcp` 3.9.0 SDK, with only the client and bounded
async-pipe transport features. No Node runtime is bundled or required by this
crate; a configured server can itself require Node, Python, or another runtime.

`McpServerConfig` stores a server identifier, executable, arguments, optional
absolute working directory, timeout, and names of approved environment variables.
It never stores environment values. Do not put credentials in arguments.
The default child environment includes only ordinary process, locale, XDG and
desktop connection variables. Additional variables must be named explicitly;
`DEEPSEEK_API_KEY` is never forwarded.
Saved working-folder paths need not remain mounted or present to open the app;
the folder is checked when the user explicitly connects the server.

The app owns command approval and individual tool-call approval. This crate is
not a process sandbox: a configured executable runs as the current OS user.
The native WASM text-extension permissions remain separate.

Discovery is frozen for the connection, bounded to 64 tools and 256 KiB of schema
and description data. Names are deterministic per server/raw tool name and are
mapped back through that catalog; they are never parsed to choose a wire tool.
Reconnect to refresh a changed catalog. Calls take an object of at most 64 KiB
and return at most 64 KiB of text/serialized structured data. A single incoming
protocol line is bounded to 1 MiB.

Images, audio, embedded resources, interactive input and background task results
are not supported by the initial text bridge. They fail explicitly. Sampling,
roots and elicitation capabilities are not granted, and server instructions are
not inserted into the model system prompt. Keep SDK protocol tracing disabled
or filter its private data when adding a logging subscriber to the application.

Cancellation and timeouts close the connection and stop its owned process group,
including descendants. `close().await` reaps the direct server child before
returning. Dropping the final clone kills the process group and schedules child
reaping while a Tokio runtime is available; application shutdown should always
call `close` explicitly before dropping the runtime. Concurrent clone closes wait
for that same completed cleanup.

On Linux, the direct server child also receives a parent-death signal if the
application exits abruptly. This is not a sandbox or a guarantee about arbitrary
descendants escaping into other process groups. Normal cancellation and shutdown
stop the owned process group; trusted server selection remains important.

Tests use a synthetic local Python stdio server, no browser, real account or model
request. The environment privacy test starts an isolated test child with fake
credentials, so the developer's real keys are neither read nor forwarded.
