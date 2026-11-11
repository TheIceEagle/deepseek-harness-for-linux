#!/usr/bin/env node
// Optional, explicitly launched compatibility host. Stdout is MCP JSON-RPC only.
import { loadCompatibility } from './runtime.mjs';

const sendRaw = process.stdout.write.bind(process.stdout);
const errorRaw = process.stderr.write.bind(process.stderr);
// Upstream plugins are trusted JavaScript, not sandboxed modules. Contain their
// ordinary logging so it cannot corrupt protocol frames or expose config values.
const discard = (_value, encoding, callback) => {
  if (typeof encoding === 'function') encoding();
  else if (typeof callback === 'function') callback();
  return true;
};
process.stdout.write = discard;
process.stderr.write = discard;

const MAX_FRAME = 1024 * 1024;
const decode = new TextDecoder('utf-8', { fatal: true });
const MAX_OUTPUT = 64 * 1024;
const MAX_TOOLS = 64;
const active = new Map();
let host;
let initialized = false;
let closing;

function optionsFrom(args) {
  const names = new Map([['--runtime', 'runtime'], ['--profile', 'profile'], ['--plugin', 'plugin'], ['--plugin-config-file', 'configFile']]);
  const result = { envNames: [] };
  for (let index = 0; index < args.length; index += 2) {
    if (args[index] === '--env-name') {
      if (!args[index + 1] || result.envNames.length >= 32) throw new Error('Invalid host arguments.');
      result.envNames.push(args[index + 1]);
      continue;
    }
    const name = names.get(args[index]);
    if (!name || result[name] !== undefined || !args[index + 1]) throw new Error('Invalid host arguments.');
    result[name] = args[index + 1];
  }
  if (!result.runtime || !result.profile || !result.plugin) throw new Error('Missing host arguments.');
  return result;
}

function send(id, result, error) {
  const frame = JSON.stringify(error ? { jsonrpc: '2.0', id, error } : { jsonrpc: '2.0', id, result });
  if (Buffer.byteLength(frame) > MAX_FRAME) throw new Error('Host response exceeded its size limit.');
  sendRaw(frame + '\n');
}

async function close(code = 0) {
  if (closing) return closing;
  closing = (async () => {
    process.stdin.pause();
    for (const request of active.values()) request.abort();
    const hardStop = setTimeout(() => process.exit(code || 1), 5000);
    try { await host?.cleanup(); }
    finally { clearTimeout(hardStop); process.exit(code); }
  })();
  return closing;
}

async function request(message) {
  const id = message?.id;
  const hasId = Number.isSafeInteger(id) || typeof id === 'string';
  if (message?.jsonrpc !== '2.0' || typeof message.method !== 'string') {
    send(null, undefined, { code: -32600, message: 'Invalid JSON-RPC request.' });
    return;
  }
  if (message.method === 'notifications/cancelled') {
    active.get(message.params?.requestId)?.abort();
    return;
  }
  if (!hasId) return;
  try {
    if (message.method === 'initialize') {
      if (initialized) throw new Error('Already initialized.');
      initialized = true;
      send(id, { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'deepseek-harness-js-compat', version: '0.2.0' } });
    } else if (!initialized) {
      send(id, undefined, { code: -32002, message: 'Initialize the compatibility host first.' });
    } else if (message.method === 'ping') {
      send(id, {});
    } else if (message.method === 'tools/list') {
      if (message.params?.cursor !== undefined) throw new Error('Unknown tool cursor.');
      const schemas = host.schemas();
      if (schemas.length > MAX_TOOLS) throw new Error('Too many plugin tools.');
      send(id, { tools: schemas.map(tool => ({ name: tool.name, description: tool.description, inputSchema: tool.parameters })) });
    } else if (message.method === 'tools/call') {
      if (active.size >= 4 || active.has(id)) throw new Error('Too many active calls.');
      const controller = new AbortController();
      active.set(id, controller);
      const timeout = setTimeout(() => controller.abort(), 60_000);
      try {
        const result = await host.call(message.params?.name, message.params?.arguments ?? {}, controller.signal);
        const text = result.isError ? 'The plugin tool call failed.' : result.content.filter(block => block.type === 'text').map(block => block.text).join('\n');
        const unsupported = result.content.some(block => block.type !== 'text');
        const content = [{ type: 'text', text: unsupported ? 'The plugin returned content that this text bridge cannot display.' : text }];
        if (Buffer.byteLength(JSON.stringify(content)) > MAX_OUTPUT) throw new Error('Plugin result too large.');
        send(id, { content, isError: Boolean(result.isError) || unsupported });
      } catch {
        send(id, { content: [{ type: 'text', text: controller.signal.aborted ? 'The plugin tool call was cancelled or timed out.' : 'The plugin tool call failed.' }], isError: true });
      } finally {
        clearTimeout(timeout);
        active.delete(id);
      }
    } else {
      send(id, undefined, { code: -32601, message: 'This compatibility host does not provide that method.' });
    }
  } catch {
    send(id, undefined, { code: -32602, message: 'The plugin request is invalid or exceeds the bridge limits.' });
  }
}

async function main() {
  const options = optionsFrom(process.argv.slice(2));
  host = await loadCompatibility(options);
  if (host.schemas().length === 0 || host.schemas().length > MAX_TOOLS) throw new Error('The plugin did not register a supported tool set.');
  // This process owns shutdown. The upstream boot installs CLI signal handlers;
  // replace those only after boot so our profile lock and Agent are released.
  for (const signal of ['SIGTERM', 'SIGINT']) {
    process.removeAllListeners(signal);
    process.once(signal, () => { void close(0); });
  }
  process.stdin.on('end', () => { void close(0); });
  process.stdin.on('error', () => { void close(1); });
  let buffer = Buffer.alloc(0);
  process.stdin.on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    let newline;
    while ((newline = buffer.indexOf(10)) >= 0) {
      if (newline > MAX_FRAME) { void close(1); return; }
      const frame = buffer.subarray(0, newline);
      buffer = buffer.subarray(newline + 1);
      if (frame.length === 0) continue;
      try { void request(JSON.parse(decode.decode(frame))); }
      catch { send(null, undefined, { code: -32700, message: 'Invalid JSON-RPC JSON.' }); }
    }
    if (buffer.length > MAX_FRAME) void close(1);
  });
  process.stdin.resume();
}

main().catch(async () => {
  errorRaw('The JavaScript compatibility host could not start. Check the pinned runtime, plugin and isolated profile.\n');
  await close(1);
});
