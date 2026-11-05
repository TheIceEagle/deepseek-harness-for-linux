import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm, mkdir, readFile, readdir, readlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { once } from 'node:events';

const root = fileURLToPath(new URL('../', import.meta.url));
const profile = await mkdtemp(join(tmpdir(), 'harness-backend-smoke-'));
const started = performance.now();
const child = spawn(join(root, 'runtime/node'), [
  join(root, 'runtime/node_modules/@deepseek-ai/dsh/lib/bin.js'),
  'web', '--no-open', '--host', '127.0.0.1', '--port', '0',
], { detached: true, cwd: profile, env: { ...process.env, DSH_HOME: join(profile, 'home') }, stdio: ['ignore', 'pipe', 'pipe'] });
child.stderr.resume();
let timer;
try {
  const launch = await new Promise((resolve, reject) => {
    let buffer = '';
    timer = setTimeout(() => reject(new Error('Harness startup timed out.')), 60000);
    child.on('error', () => reject(new Error('Harness process could not launch.')));
    child.on('exit', code => reject(new Error(`Harness exited before readiness (code ${code}).`)));
    child.stdout.on('data', chunk => {
      buffer += chunk.toString();
      for (let newline; (newline = buffer.indexOf('\n')) !== -1;) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        const match = line.match(/dsh web: (http:\/\/127\.0\.0\.1:\d+\/\?\S+)/);
        if (match) resolve(match[1]);
      }
      if (buffer.length > 65536) buffer = '';
    });
  });
  clearTimeout(timer);
  const startupMs = Math.round(performance.now() - started);
  const commandLine = await readFile(`/proc/${child.pid}/cmdline`, 'utf8');
  assert.equal(commandLine.includes('token='), false, 'The backend arguments contain no launch credential');
  const descriptors = await readdir(`/proc/${child.pid}/fd`);
  const links = await Promise.all(descriptors.map(descriptor => readlink(`/proc/${child.pid}/fd/${descriptor}`).catch(() => '')));
  const socketInodes = new Set(links.map(link => link.match(/^socket:\[(\d+)\]$/)?.[1]).filter(Boolean));
  const launchPort = Number(new URL(launch).port);
  const bindings = [];
  for (const family of ['tcp', 'tcp6']) {
    const table = await readFile(`/proc/${child.pid}/net/${family}`, 'utf8');
    for (const line of table.trim().split('\n').slice(1)) {
      const fields = line.trim().split(/\s+/);
      const [address, port] = fields[1].split(':');
      if (fields[3] === '0A' && Number.parseInt(port, 16) === launchPort && socketInodes.has(fields[9])) {
        bindings.push({ family, address });
      }
    }
  }
  assert.equal(bindings.length, 1, 'The announced HTTP listener belongs to this backend');
  assert.equal(bindings[0].family === 'tcp' && bindings[0].address === '0100007F', true,
    'The actual HTTP socket listens only on IPv4 127.0.0.1');
  const response = await fetch(launch, { redirect: 'manual' });
  const cookies = response.headers.getSetCookie().map(cookie => cookie.split(';')[0]).join('; ');
  assert.ok(cookies, 'Launch establishes an authenticated cookie');
  const cleanUrl = new URL('/', launch);
  const page = await fetch(cleanUrl, { headers: { Cookie: cookies } });
  assert.equal(page.status, 200);
  const html = await page.text();
  const scriptPath = html.match(/<script[^>]+src="([^"]+)"/)?.[1]?.replaceAll('&amp;', '&');
  assert.ok(scriptPath, 'Workspace includes an application script');
  const script = await fetch(new URL(scriptPath, cleanUrl), { headers: { Cookie: cookies } });
  assert.equal(script.status, 200, `Application script ${new URL(scriptPath, cleanUrl).pathname} is available`);
  const stopStarted = performance.now();
  const exited = once(child, 'exit');
  process.kill(-child.pid, 'SIGTERM');
  const [exitCode] = await exited;
  assert.equal(exitCode, 0, 'Harness exits gracefully');
  const report = { runtime: '0.2.1-rc.5', node: process.version, startupMs, loopbackOnly: true, credentialFreeArguments: true, authenticatedPage: page.status, script: script.status, gracefulExitCode: exitCode, shutdownMs: Math.round(performance.now() - stopStarted) };
  await mkdir(join(root, 'artifacts'), { recursive: true });
  await writeFile(join(root, 'artifacts/backend-smoke.json'), `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify(report));
} finally {
  clearTimeout(timer);
  try { process.kill(-child.pid, 'SIGKILL'); } catch {}
  await rm(profile, { recursive: true, force: true });
}
