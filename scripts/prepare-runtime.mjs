import { access, chmod, copyFile, readFile, realpath } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const runtime = join(root, 'runtime');
await access(join(runtime, 'node_modules/@deepseek-ai/dsh/lib/bin.js')).catch(() => {
  throw new Error('Install the Harness runtime first: npm ci --prefix runtime');
});
await access(join(runtime, 'node_modules/pnpm/bin/pnpm.mjs')).catch(() => {
  throw new Error('Install the bundled package manager first: npm ci --prefix runtime');
});
await access(join(runtime, 'node_modules/pnpm/LICENSE')).catch(() => {
  throw new Error('The bundled package manager license is missing. Run npm ci --prefix runtime.');
});
const pnpm = JSON.parse(await readFile(join(runtime, 'node_modules/pnpm/package.json'), 'utf8'));
const dependencies = JSON.parse(await readFile(join(runtime, 'package.json'), 'utf8')).dependencies;
if (pnpm.version !== dependencies.pnpm) {
  throw new Error('The installed pnpm version does not match the runtime pin. Run npm ci --prefix runtime.');
}
if (process.platform !== 'linux' || process.arch !== 'x64') {
  throw new Error('This package currently targets Linux x86_64. Prepare a matching Node runtime for other platforms.');
}
const destination = join(runtime, 'node');
const source = await realpath(process.execPath);
const licenseDestination = join(runtime, 'NODE-LICENSE');
const licenseSource = source === destination ? licenseDestination : join(dirname(source), '..', 'LICENSE');
await access(licenseSource).catch(() => {
  throw new Error('The Node.js LICENSE file is missing. Build with an official Node.js distribution so its license and third-party notices can be bundled.');
});
if (source !== destination) {
  await copyFile(source, destination);
  await copyFile(licenseSource, licenseDestination);
}
await chmod(destination, 0o755);
console.log(`Prepared Node ${process.version}, pnpm ${pnpm.version}, their license notices, and DeepSeek Harness 0.2.1-rc.5 for Linux x86_64.`);
