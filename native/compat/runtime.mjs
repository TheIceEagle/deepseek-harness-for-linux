import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { readFile, writeFile, mkdir, lstat, readdir, realpath, unlink, symlink, chmod } from 'node:fs/promises';
import { join, resolve, dirname, isAbsolute } from 'node:path';
import { randomUUID } from 'node:crypto';

export const UPSTREAM_VERSION = '0.2.1-rc.5';

export function scrubEnvironment(environment, approvedNames = []) {
  if (!Array.isArray(approvedNames) || approvedNames.length > 32 || approvedNames.some(name =>
    typeof name !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(name)
    || name.toUpperCase() === 'DEEPSEEK_API_KEY' || /^DSH_/i.test(name))) {
    throw new Error('Invalid approved environment variable name.');
  }
  const approved = new Set(approvedNames);
  const kept = {};
  for (const [name, value] of Object.entries(environment)) {
    if (value === undefined || name.toUpperCase() === 'DEEPSEEK_API_KEY' || /^DSH_/i.test(name)) continue;
    if (!/KEY|PASSWORD|SECRET|TOKEN/i.test(name) || approved.has(name)) kept[name] = value;
  }
  return kept;
}

export async function loadCompatibility(options) {
  const retained = scrubEnvironment(process.env, options.envNames);
  const runtime = await realpath(options.runtime);
  const require = createRequire(join(runtime, 'package.json'));
  const anchor = require.resolve('@deepseek-ai/dsh/package.json');
  const installed = JSON.parse(await readFile(anchor, 'utf8'));
  if (installed.version !== UPSTREAM_VERSION) throw new Error('Unsupported Harness runtime version.');
  const profile = resolve(options.profile);
  if (!isAbsolute(options.profile)) throw new Error('Use an absolute compatibility profile path.');
  await mkdir(profile, { recursive: true, mode: 0o700 });
  if (!(await lstat(profile)).isDirectory() || (await lstat(profile)).isSymbolicLink()) throw new Error('Invalid compatibility profile.');
  const marker = join(profile, 'native-compat.json');
  if (!(await readdir(profile)).includes('native-compat.json')) {
    if ((await readdir(profile)).length) throw new Error('Choose an empty compatibility profile.');
    await writeFile(marker, JSON.stringify({ version: 1, runtimeVersion: UPSTREAM_VERSION }) + '\n', { flag: 'wx', mode: 0o600 });
  }
  const markerInfo = await lstat(marker);
  if (!markerInfo.isFile() || markerInfo.isSymbolicLink() || markerInfo.size > 1024) throw new Error('Invalid compatibility profile marker.');
  const marked = JSON.parse(await readFile(marker, 'utf8'));
  if (marked.version !== 1 || marked.runtimeVersion !== UPSTREAM_VERSION) throw new Error('Invalid compatibility profile marker.');
  await chmod(profile, 0o700);
  const lock = join(profile, 'native-compat.lock');
  try {
    await writeFile(lock, String(process.pid), { flag: 'wx', mode: 0o600 });
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
    const lockInfo = await lstat(lock);
    if (!lockInfo.isFile() || lockInfo.isSymbolicLink() || lockInfo.size > 20) throw new Error('Compatibility profile is locked.');
    const owner = Number(await readFile(lock, 'utf8'));
    if (!Number.isInteger(owner) || owner < 1) throw new Error('Compatibility profile is locked.');
    try { process.kill(owner, 0); throw new Error('Compatibility profile is already open.'); }
    catch (probe) { if (probe.code !== 'ESRCH') throw probe; }
    await unlink(lock);
    await writeFile(lock, String(process.pid), { flag: 'wx', mode: 0o600 });
  }
  let shutdown;
  let handle;
  const cleanup = async () => {
    try { await handle?.dispose(); } finally {
      try { await shutdown?.shutdown(0); } finally {
        if (await readFile(lock, 'utf8').catch(() => '') === String(process.pid)) await unlink(lock).catch(() => {});
      }
    }
  };
  try {
    for (const key of Object.keys(process.env)) if (!(key in retained)) delete process.env[key];
    Object.assign(process.env, {
      DSH_HOME: profile,
      DSH_AGENTS_HOME: join(profile, 'agents'),
      DSH_TELEMETRY_DISABLED: '1',
      DSH_PERMISSION_MODE: 'workspace-write',
    });
    const importPackage = async name => import(pathToFileURL(require.resolve(name)).href);
    const boot = await importPackage('@deepseek-ai/dsh-app-boot');
    const { createLaunchEnvironmentSnapshot } = await importPackage('@deepseek-ai/dsh-launch-environment');
    const { runProfile } = await importPackage('@deepseek-ai/dsh/profile-boot');
    const dir = join(profile, 'profiles', 'native-compat');
    boot.initProfile(dir, ['@deepseek-ai/dsh-base']);
    const directFolder = isAbsolute(options.plugin) && (await lstat(options.plugin)).isDirectory();
    const pluginPath = directFolder ? undefined : require.resolve(options.plugin);
    let pluginRoot = directFolder ? await realpath(options.plugin) : dirname(pluginPath);
    for (let current = pluginRoot; dirname(current) !== current; current = dirname(current)) {
      try { await lstat(join(current, 'package.json')); pluginRoot = current; break; } catch {}
    }
    let manifest = {};
    try { manifest = JSON.parse(await readFile(join(pluginRoot, 'package.json'), 'utf8')); } catch {}
    if (boot.evaluatePluginCompatibility(manifest, {}, UPSTREAM_VERSION)) throw new Error('The plugin requires a different Harness runtime.');
    const packageName = manifest.name ?? 'native-selected-plugin';
    if (!/^(@[a-zA-Z0-9_-]+\/)?[a-zA-Z0-9_-][a-zA-Z0-9._-]*$/.test(packageName)) throw new Error('Invalid plugin package name.');
    boot.writeProfileManifest(dir, { private: true, type: 'module', dependencies: { [packageName]: 'file:' + pluginRoot }, dsh: { profile: { bundles: ['@deepseek-ai/dsh-base'] } } });
    const link = join(dir, 'node_modules', packageName);
    await mkdir(dirname(link), { recursive: true, mode: 0o700 });
    await unlink(link).catch(error => { if (error.code !== 'ENOENT') throw error; });
    await symlink(pluginRoot, link, 'dir');
    const composition = boot.loadProfileDirectory('native-compat', dir, anchor, { userLayer: false });
    const entries = boot.composeEntries(composition.layers.map(layer => layer.patches));
    const disable = entries.filter(entry => (entry.name?.startsWith('@deepseek-ai/dsh-tool-') && entry.id !== 'timeout-policy') || entry.name?.endsWith('/tools'))
      .map(entry => ({ id: entry.id, disabled: true }));
    disable.push({ id: 'hmr', disabled: true }, { id: 'plugin-manager', disabled: true });
    await writeFile(composition.patchPath, JSON.stringify(disable) + '\n', { mode: 0o600 });
    composition.patches = disable;
    const instance = await runProfile({
      environment: createLaunchEnvironmentSnapshot([{ source: 'process', values: process.env }]),
      profile: 'native-compat',
      resolvedProfile: { profile: composition, installAnchor: anchor },
      patchFiles: [], args: [],
    });
    shutdown = instance.shutdown;
    const ctx = instance.ctx;
    if (!ctx.get('tools') || !ctx.get('agents')) throw new Error('Harness tool services did not start.');
    const baseline = new Set(ctx.tools.schemas().map(tool => tool.name));
    if (options.configFile && (await lstat(options.configFile)).size > 64 * 1024) throw new Error('Plugin configuration is too large.');
    const config = options.configFile ? JSON.parse(await readFile(options.configFile, 'utf8')) : undefined;
    if (manifest.dsh?.bundle) {
      const patches = boot.bundlePatchPaths(pluginRoot, manifest.dsh.bundle).flatMap(path => boot.loadOverlayPatches('native-compat', path));
      if (config !== undefined && !Array.isArray(config)) throw new Error('Bundle configuration must be a JSON Cordis patch list.');
      await boot.recomposeProfile(ctx, [...composition.layers.flatMap(layer => layer.patches), ...disable, ...patches, ...(config ?? []), { id: 'hmr', disabled: true }, { id: 'plugin-manager', disabled: true }], 'native-compat');
    } else {
      const plugin = await import(pathToFileURL(pluginPath ?? require.resolve(pluginRoot)).href);
      if (config !== undefined && (!config || typeof config !== 'object' || Array.isArray(config))) throw new Error('The plugin configuration must be a JSON object.');
      const fiber = ctx.plugin(plugin.default ?? plugin, config ?? {});
      await fiber;
    }
    handle = await ctx.agents.create({ sessionId: randomUUID(), meta: { cwd: process.cwd() } });
    const schemas = () => ctx.tools.schemas(handle.agent).filter(tool => !baseline.has(tool.name));
    const allowed = new Set(schemas().map(tool => tool.name));
    return {
      schemas: () => schemas().filter(tool => allowed.has(tool.name)),
      async call(name, args, signal) {
        if (!allowed.has(name)) throw new Error('The tool is not provided by the selected plugin.');
        return ctx.tools.execute({ callId: randomUUID(), name, arguments: args, agent: handle.agent, signal });
      },
      cleanup,
    };
  } catch (error) {
    await cleanup();
    throw error;
  }
}
