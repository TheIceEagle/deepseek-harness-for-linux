import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { chmod, mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { By, Key } from 'selenium-webdriver';

const providerId = 'harness-local-workflow';
const modelId = 'harness-workflow';
const bundleName = 'dsh-linux-smoke-plugin';
const streamPrompt = 'HARNESS_WORKFLOW_STREAM: exercise the local streaming fixture.';
const toolPrompt = 'HARNESS_WORKFLOW_TOOLS: read the fixture and write its result.';
const cancelPrompt = 'HARNESS_WORKFLOW_CANCEL: exercise the local cancellation fixture.';
const streamFirst = 'Harness local streaming started';
const streamLast = 'Harness local streaming completed';
const toolLast = 'Harness local file tools completed';
const cancelFirst = 'Harness local cancellation started';
const fileContents = 'Harness desktop workflow fixture\n';
const outputContents = 'Harness desktop write tool verified\n';

// The provider is the only stand-in. Session creation, streaming, tools,
// cancellation, persistence and package management run through upstream Harness.
export async function prepareWorkflow({ scratch, data, env }) {
  const profile = join(data, 'local.deepseek.harness-linux', 'harness');
  const workspace = join(scratch, 'Documents', 'deepseek-harness', 'default-workspace');
  const bundle = join(scratch, bundleName);
  const hostTools = join(scratch, 'unavailable-host-tools');
  const hostToolMarker = join(scratch, 'host-tool-was-used');
  const stats = { requests: 0, toolCalls: [], cancellationStarted: false, cancellationDisconnected: false };
  const timers = new Set();
  const failures = [];
  let finishStreaming;
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.headers.authorization === 'Bearer harness-local-fixture', true, 'The local provider receives only its synthetic fixture credential');
      if (request.method === 'GET' && request.url === '/v1/models') {
        response.setHeader('content-type', 'application/json');
        response.end(JSON.stringify({ object: 'list', data: [{ id: modelId, object: 'model', owned_by: providerId }] }));
        return;
      }
      assert.equal(request.method, 'POST');
      assert.equal(request.url, '/v1/chat/completions');
      let body = '';
      for await (const chunk of request) {
        body += chunk;
        assert.ok(body.length <= 2 * 1024 * 1024, 'The local provider request is bounded');
      }
      const input = JSON.parse(body);
      assert.equal(input.model, modelId);
      assert.equal(input.stream, true);
      stats.requests++;
      // Harness may append a separate user-context message after the submitted
      // prompt. Find the latest fixture task rather than assuming it is last.
      const promptIndex = input.messages.findLastIndex(message => JSON.stringify(message.content).includes('HARNESS_WORKFLOW_'));
      const prompt = JSON.stringify(input.messages[promptIndex]?.content ?? '');
      const toolResults = input.messages.slice(promptIndex + 1).filter(message => message.role === 'tool');
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      response.flushHeaders();
      const chunk = (delta, finishReason = null) => {
        if (!response.destroyed) response.write(`data: ${JSON.stringify({ id: 'harness-local-completion', object: 'chat.completion.chunk', created: 1, model: modelId, choices: [{ index: 0, delta, finish_reason: finishReason }] })}\n\n`);
      };
      const finish = () => {
        chunk({}, 'stop');
        response.end('data: [DONE]\n\n');
      };
      chunk({ role: 'assistant' });
      if (prompt.includes('HARNESS_WORKFLOW_CANCEL')) {
        stats.cancellationStarted = true;
        chunk({ content: cancelFirst });
        const timer = setInterval(() => chunk({ content: ' waiting' }), 200);
        timers.add(timer);
        response.on('close', () => {
          clearInterval(timer);
          timers.delete(timer);
          stats.cancellationDisconnected = true;
        });
        return;
      }
      if (prompt.includes('HARNESS_WORKFLOW_TOOLS') && toolResults.length < 2) {
        const name = toolResults.length === 0 ? 'read' : 'write';
        assert.ok(input.tools.some(tool => tool.function?.name === name), `The installed runtime advertises its real ${name} tool`);
        if (name === 'write') assert.ok(JSON.stringify(toolResults).includes(fileContents.trim()), 'The real read tool returned the fixture content to the model');
        const args = name === 'read'
          ? { file_path: join(workspace, 'fixture.txt') }
          : { file_path: join(workspace, 'output.txt'), content: outputContents };
        stats.toolCalls.push(name);
        chunk({ tool_calls: [{ index: 0, id: `harness-local-${name}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] });
        chunk({}, 'tool_calls');
        response.end('data: [DONE]\n\n');
        return;
      }
      if (prompt.includes('HARNESS_WORKFLOW_TOOLS')) {
        assert.equal(toolResults.length, 2, 'Both real filesystem tools returned to the model');
        chunk({ content: toolLast });
        finish();
        return;
      }
      assert.ok(prompt.includes('HARNESS_WORKFLOW_STREAM'), 'Only fixture prompts reach the local provider');
      chunk({ content: `${streamFirst}\n\n` });
      // Keep completion gated on the observed UI fragment, independent of CI
      // speed, so this proves streaming rather than a fast completed response.
      await new Promise(resolve => { finishStreaming = resolve; });
      chunk({ content: streamLast });
      finish();
    } catch (error) {
      // Keep request bodies and credentials out of reports even on failure.
      failures.push(error instanceof Error ? error.message : 'Local provider failed');
      response.destroy();
    }
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  try {
    await mkdir(profile, { recursive: true });
    await mkdir(workspace, { recursive: true });
    await mkdir(bundle, { recursive: true });
    await mkdir(hostTools, { recursive: true });
    for (const tool of ['node', 'pnpm']) {
      const path = join(hostTools, tool);
      await writeFile(path, `#!/bin/sh\nprintf '%s\\n' '${tool}' >> '${hostToolMarker}'\nexit 71\n`);
      await chmod(path, 0o755);
    }
    env.PATH = `${hostTools}:${env.PATH ?? '/usr/bin:/bin'}`;
    await writeFile(join(workspace, 'fixture.txt'), fileContents);
    await writeFile(join(bundle, 'package.json'), `${JSON.stringify({ name: bundleName, version: '1.0.0', type: 'module', main: './index.mjs', exports: { '.': './index.mjs' }, license: 'MIT', engines: { dsh: '0.2.1-rc.5' }, dsh: { bundle: { patch: './cordis.patch.yml' } } }, null, 2)}\n`);
    await writeFile(join(bundle, 'index.mjs'), 'export function apply() {}\n');
    await writeFile(join(bundle, 'cordis.patch.yml'), `- insert:\n    - id: linux-smoke-plugin\n      name: ${bundleName}\n`);
    await writeFile(join(profile, 'cordis.patch.yml'), `- id: llm-deepseek\n  disabled: true\n- id: llm-deepseek-account\n  disabled: true\n- id: session-title-llm\n  disabled: true\n- id: agent-default-model\n  config:\n    provider: ${providerId}\n    model: ${modelId}\n- id: llm-pi-ai\n  config:\n    providers:\n      ${providerId}:\n        displayName: Local workflow fixture\n        api: openai-completions\n        baseURL: http://127.0.0.1:${address.port}/v1\n        headers:\n          Authorization: Bearer harness-local-fixture\n        retryPolicy:\n          mode: normal\n          maxRetries: 0\n        models:\n          - id: ${modelId}\n            name: Local workflow fixture\n            contextWindow: 32768\n            maxTokens: 4096\n`);
    // Fresh package-manager state, independent of the developer's npm/pnpm config.
    env.NPM_CONFIG_USERCONFIG = join(scratch, 'npmrc');
    env.XDG_STATE_HOME = join(scratch, 'state');
    await writeFile(env.NPM_CONFIG_USERCONFIG, 'registry=https://registry.npmjs.org/\n');
  } catch (error) {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    throw error;
  }
  return {
    async run(driver, { restartBackend } = {}) {
      await driver.manage().setTimeouts({ script: 120000 });
      const rpc = async (endpoint, args = {}) => {
        const result = await driver.executeAsyncScript(`
          const [endpoint, args, done] = arguments;
          const rpcId = crypto.randomUUID();
          fetch('/api/' + endpoint, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ type: 'client-request', rpcId, method: endpoint, payload: { args } }) })
            .then(response => response.json()).then(response => done(response.result)).catch(() => done({ ok: false, error: { code: 'fixture-transport', message: 'Fixture request failed' } }));
        `, endpoint, args);
        assert.equal(result.ok, true, `${endpoint} succeeds: ${JSON.stringify(result.error ?? {})}`);
        return result.value;
      };
      const bodyHas = text => {
        assert.deepEqual(failures, [], 'The local provider accepts the upstream request');
        return driver.executeScript('return Boolean(document.body?.innerText.includes(arguments[0]))', text).catch(error => {
          if (error.name === 'NoSuchFrameError') return false;
          throw error;
        });
      };
      const submit = async prompt => {
        const editor = await driver.wait(async () => {
          const editors = await driver.findElements(By.css('[contenteditable="true"]'));
          for (const candidate of editors) if (await candidate.isDisplayed() && await candidate.isEnabled()) return candidate;
          return false;
        }, 20000, 'The upstream conversation composer is available');
        await editor.sendKeys(prompt);
        await editor.sendKeys(Key.ENTER);
      };
      const noStopButton = async () => (await driver.findElements(By.css('button[aria-label="Stop generating"]'))).length === 0;
      const openSavedSession = async sessionId => {
        await driver.wait(async () => {
          try {
            const rows = await driver.findElements(By.css(`[data-row-key="session:${sessionId}"]`));
            if (rows.length === 0 || !await rows[0].isDisplayed()) return false;
            await rows[0].click();
            return true;
          } catch (error) {
            if (error.name === 'StaleElementReferenceError' || error.name === 'ElementNotInteractableError' || error.name === 'NoSuchFrameError') return false;
            throw error;
          }
        }, 20000, 'The saved Session remains available in the upstream sidebar');
        await driver.wait(() => bodyHas(streamLast), 20000, 'Opening the saved Session restores its completed response');
      };
      console.log('Testing a real upstream conversation against a local provider.');
      // Exercise the normal first-use acknowledgement in this disposable
      // profile before interacting with the composer behind its modal.
      if (await bodyHas('Preview Notice')) {
        await driver.findElement(By.xpath('//button[normalize-space(.)="Continue"]')).click();
        await driver.wait(async () => !await bodyHas('Preview Notice'), 10000, 'The first-use preview notice closes');
      }
      await submit(streamPrompt);
      await driver.wait(() => bodyHas(streamFirst), 20000, 'The first streamed fragment appears before completion');
      assert.equal(await bodyHas(streamLast), false, 'The UI displays a partial streamed response');
      assert.ok(finishStreaming, 'The provider is waiting for the streamed UI acknowledgement');
      finishStreaming();
      await driver.wait(() => bodyHas(streamLast), 20000, 'The completed response appears in the real chat');
      await driver.wait(noStopButton, 15000, 'The first turn settles');
      const sessionId = await driver.executeScript('return document.querySelector("[data-conversation-session]")?.dataset.conversationSession');
      assert.ok(sessionId, 'The upstream UI created a real Session');
      await submit(toolPrompt);
      await driver.wait(() => bodyHas(toolLast), 30000, 'The model completed real filesystem tools');
      await driver.wait(noStopButton, 15000, 'The tool turn settles');
      assert.deepEqual(stats.toolCalls, ['read', 'write']);
      assert.equal(await readFile(join(workspace, 'output.txt'), 'utf8'), outputContents, 'The real upstream write tool created the requested file');
      await submit(cancelPrompt);
      await driver.wait(() => bodyHas(cancelFirst), 20000, 'The cancellable stream reaches the UI');
      await driver.findElement(By.css('button[aria-label="Stop generating"]')).click();
      await driver.wait(() => stats.cancellationDisconnected, 15000, 'Stopping the real conversation aborts the provider connection');
      await driver.wait(noStopButton, 15000, 'The cancellation settles');
      assert.equal(stats.cancellationStarted, true);
      const projection = await rpc('session/projections', { request: { sessionId } });
      assert.equal(projection.kind, 'sequenced');
      const history = await rpc('session/page', { request: { address: { kind: 'session', sessionId }, throughSeq: projection.asOfSeq, maxMessages: 100 } });
      assert.ok(JSON.stringify(history).includes(streamLast), 'The completed response is in durable Session history');
      assert.ok(JSON.stringify(history).includes('aborted'), 'The durable history records cancellation');
      await driver.navigate().refresh();
      await openSavedSession(sessionId);
      await driver.wait(() => bodyHas(toolLast), 20000, 'Reloading restores completed tool work');
      assert.equal(await driver.executeScript('return document.querySelector("[data-conversation-session]")?.dataset.conversationSession'), sessionId, 'The same Session reopens after a renderer reload');
      if (restartBackend) {
        await restartBackend();
        // A restarted backend uses a new origin. Upstream's selected view may
        // reset, while its durable Session must still open from the sidebar.
        await openSavedSession(sessionId);
        await driver.wait(() => bodyHas(toolLast), 20000, 'Saved tool results survive a backend restart');
        assert.equal(await driver.executeScript('return document.querySelector("[data-conversation-session]")?.dataset.conversationSession'), sessionId);
      }
      console.log('Testing local plugin installation through the upstream plugin manager.');
      const installed = await rpc('pluginManager/installBundle', { spec: bundle, options: { enabled: true, saveExact: true } });
      assert.equal(installed.application, 'applied', `The harmless bundle activates without restarting: ${JSON.stringify(installed.error ?? {})}`);
      assert.equal(installed.packageResult?.exitCode, 0, 'The bundled package manager installed the local fixture');
      const bundles = await rpc('pluginManager/listBundles');
      assert.ok(bundles.some(item => item.name === bundleName && item.installed && item.enabled && !item.error), 'The fixture bundle is installed and enabled');
      await driver.wait(async () => {
        const plugins = await rpc('pluginManager/listPlugins');
        return plugins.some(item => item.moduleName === bundleName && item.enabled && item.fiberPhase === 'active');
      }, 15000, 'The fixture plugin becomes active in the running upstream composition');
      const removed = await rpc('pluginManager/removeBundle', { name: bundleName });
      assert.equal(removed.application, 'applied', 'The harmless bundle removes without restarting');
      assert.equal((await rpc('pluginManager/listBundles')).some(item => item.name === bundleName && item.installed), false, 'The fixture bundle is absent after removal');
      assert.equal(await readFile(hostToolMarker, 'utf8').catch(error => {
        if (error.code === 'ENOENT') return null;
        throw error;
      }), null, 'The installed app uses bundled Node and pnpm instead of host developer tools');
      assert.deepEqual(failures, [], 'The local provider had no protocol or fixture failures');
      await driver.manage().setTimeouts({ script: 10000 });
      return { provider: 'loopback OpenAI-compatible fixture; no paid model request', uiSendAndStreaming: 'passed', uiCancellation: 'passed', filesystemReadWrite: 'passed', savedSessionReload: 'passed', savedSessionBackendRestart: restartBackend ? 'passed' : 'not run', localPluginInstallActivateRemove: 'passed', providerRequests: stats.requests };
    },
    async close() {
      finishStreaming?.();
      for (const timer of timers) clearInterval(timer);
      server.closeAllConnections();
      await new Promise(resolve => server.close(resolve));
    },
  };
}
