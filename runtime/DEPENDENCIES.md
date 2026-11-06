# Runtime dependency overrides

`package.json` pins the Harness runtime to `@deepseek-ai/dsh@0.2.1-rc.5`.
The `overrides` entries keep its transitive dependencies on reviewed patched
versions while that upstream release still requests older versions.

| Override | Why it exists for the pinned Harness release |
| --- | --- |
| `undici: 8.12.2` | Harness HTTP proxy/fetch packages pin `8.11.2`. The [8.12.2 security release](https://github.com/nodejs/undici/releases/tag/v8.12.2) includes TLS certificate-validation and denial-of-service fixes. |
| `zod: 4.6.7` | Many Harness packages pin `4.4.3`. This pin includes the [early collection-size checks](https://github.com/colinhacks/zod/pull/6652) that reject oversized bounded arrays, sets, and maps before parsing every element. Unbounded schemas still require appropriate input limits. |
| `fflate: 0.8.3` | `@deepseek-ai/libreoffice-kit` pins `0.8.2`. [0.8.3](https://github.com/101arrowz/fflate/releases/tag/v0.8.3) fixes ZIP64 extra-field parsing, including a buffer over-read. |
| `@modelcontextprotocol/client: 2.3.0` | `@deepseek-ai/dsh-mcp-client` pins `2.2.0`. [GHSA-6prh-2h8m-c8cw](https://github.com/modelcontextprotocol/typescript-sdk/security/advisories/GHSA-6prh-2h8m-c8cw) is patched in `2.3.0`, preventing cross-origin redirects from forwarding custom headers and OAuth request bodies. |

For each entry, remove it only after the selected upstream Harness release
resolves a compatible patched version without the override. Regenerate the
runtime lockfile, inspect the resolved dependency tree, run `npm audit` in this
directory, and pass the backend and native desktop smoke tests. Removing the MCP
override also requires checking the HTTP MCP/OAuth behavior; the desktop smoke
tests do not make MCP requests.

A clean audit alone is not a reason to remove a pin: it describes the overridden
dependency tree. Dependabot proposes root npm, runtime npm, Cargo, and GitHub
Actions updates for review; no automatic merging is configured. Runtime updates
must also review the exact native-package versions in `allowScripts` and run the
smoke tests without globally enabling install scripts.

## Bundled plugin package manager

The desktop runtime also pins `pnpm@11.28.11`. The pinned Harness plugin manager
uses pnpm for installation, removal, and registry inspection. Bundling its
JavaScript distribution beside Node avoids requiring a separate developer-tool
installation and retains the pnpm 11 build-script approval behavior understood
by this Harness release.

`launch.mjs` supplies the executable through Harness's public packaged-launcher
API. Package operations use the bundled Node executable and pnpm entry point;
only their `PATH` is added to Harness's scrubbed subprocess environment. The
backend also puts its bundled Node first on its own `PATH` for Node-based tools.
The desktop session's environment is unchanged. Update this pin with the runtime
audit and the installed-package workflow test, including plugin installation,
removal, and the existing dependency-script approval policy.
