import { access, chmod, copyFile, realpath } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const runtime = join(root, 'runtime');
await access(join(runtime, 'node_modules/@deepseek-ai/dsh/lib/bin.js')).catch(() => {
  throw new Error('Install the Harness runtime first: npm ci --prefix runtime');
});
if (process.platform !== 'linux' || process.arch !== 'x64') {
  throw new Error('This package currently targets Linux x86_64. Prepare a matching Node runtime for other platforms.');
}
const destination = join(runtime, 'node');
const source = await realpath(process.execPath);
if (source !== destination) await copyFile(source, destination);
await chmod(destination, 0o755);
console.log(`Prepared Node ${process.version} and DeepSeek Harness 0.2.1-rc.5 for Linux x86_64.`);
