import { delimiter } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runCli } from './node_modules/@deepseek-ai/dsh/lib/bin.js';

const runtime = fileURLToPath(new URL('.', import.meta.url));
const pnpm = fileURLToPath(new URL('./node_modules/pnpm/bin/pnpm.mjs', import.meta.url));

// Use Harness's packaged-launcher API so package operations use our pinned
// Node and pnpm without depending on the user's developer tools. Forward only
// PATH here: forwarding the whole environment would undo Harness's secret scrub.
await runCli({
  packageManager: {
    command: process.execPath,
    args: [pnpm],
    env: { PATH: [runtime, process.env.PATH].filter(Boolean).join(delimiter) },
  },
});
