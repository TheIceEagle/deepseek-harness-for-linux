import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { access, mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { Builder, By } from 'selenium-webdriver';

const root = fileURLToPath(new URL('../', import.meta.url));
const application = process.env.HARNESS_TEST_APPLICATION || join(root, 'src-tauri/target/debug/harness-for-linux');
const scratch = await mkdtemp(join(tmpdir(), 'harness-desktop-smoke-'));
const data = join(scratch, 'data');
const config = join(scratch, 'config');
const documents = join(scratch, 'Documents');
const display = ':97';
const artifacts = join(root, 'artifacts');
await mkdir(artifacts, { recursive: true });
await access(application);
await mkdir(config, { recursive: true });
await mkdir(documents, { recursive: true });
await writeFile(join(config, 'user-dirs.dirs'), `XDG_DOCUMENTS_DIR="${documents}"\n`);
const xvfb = spawn('Xvfb', [display, '-screen', '0', '1440x1000x24'], { stdio: 'ignore' });
await delay(300);
const env = { ...process.env, DISPLAY: display, GDK_BACKEND: 'x11', XDG_DATA_HOME: data, XDG_CONFIG_HOME: config, XDG_CACHE_HOME: join(scratch, 'cache') };
if (process.env.HARNESS_TEST_FAILURE === '1') env.HARNESS_TEST_RUNTIME_DIR = join(scratch, 'missing-runtime');
// A separate session bus prevents the single-instance plugin from connecting
// this test to an app the user already has open on their desktop.
const server = spawn('dbus-run-session', ['--', join(homedir(), '.cargo/bin/tauri-driver'), '--port', '4446', '--native-port', '4447'], { env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
let launchCredentialPrinted = false;
let outputTail = '';
const hasLaunchCredential = text => /[?&]token=|dsh web: /i.test(text);
const inspectOutput = chunk => {
  const text = outputTail + chunk.toString();
  launchCredentialPrinted ||= hasLaunchCredential(text);
  // Only retain enough to detect an announcement split across chunks. Never
  // print or persist this output: a failed secrecy check must not leak it itself.
  outputTail = text.slice(-256);
};
server.stdout.on('data', inspectOutput);
server.stderr.on('data', inspectOutput);
let driver;
let appPid;
try {
  for (let i = 0; i < 60; i++) {
    try { if ((await fetch('http://127.0.0.1:4446/status')).ok) break; } catch {}
    if (i === 59) throw new Error('Desktop test driver did not start.');
    await delay(100);
  }
  const started = performance.now();
  console.log('Launching the native desktop test.');
  driver = await new Builder().withCapabilities({ browserName: 'wry', pageLoadStrategy: 'none', 'tauri:options': { application } }).usingServer('http://127.0.0.1:4446/').build();
  await driver.manage().setTimeouts({ implicit: 0, pageLoad: 15000, script: 10000 });
  console.log('Native desktop test connected.');
  await writeFile(join(artifacts, 'test-session-id'), (await driver.getSession()).getId());
  const testRows = execFileSync('ps', ['-eo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).trim().split('\n').map(line => {
    const [pid, ppid, comm] = line.trim().split(/\s+/); return { pid: Number(pid), ppid: Number(ppid), comm };
  });
  const testTree = new Set([server.pid]);
  for (let i = 0; i < 8; i++) for (const row of testRows) if (testTree.has(row.ppid)) testTree.add(row.pid);
  appPid = testRows.find(row => testTree.has(row.pid) && /harness-for-lin/.test(row.comm))?.pid;
  assert.ok(appPid, 'Only the app launched by this test may be inspected or stopped');
  if (process.env.HARNESS_TEST_FAILURE === '1') {
    const retryReady = async () => {
      try { return await driver.executeScript("const retry = document.querySelector('#retry'); return Boolean(retry && !retry.hidden && !retry.disabled && document.body.dataset.state === 'error')"); }
      catch { return false; }
    };
    await driver.wait(retryReady, 15000);
    assert.match(await driver.findElement(By.id('detail')).getText(), /runtime is missing/);
    const coreDenied = await driver.executeAsyncScript(`const done = arguments[arguments.length - 1]; if (!window.__TAURI__?.core?.invoke) done('missing-api'); else window.__TAURI__.core.invoke('plugin:window|title', { label: 'main' }).then(() => done('allowed')).catch(error => done(/not allowed|permissions|denied/i.test(String(error)) ? 'denied' : 'other-error'));`);
    assert.equal(coreDenied, 'denied', 'The bundled startup page has no unused core window privileges');
    console.log('Missing-runtime error is visible.');
    await driver.executeScript("window.__retryClicks = 0; document.querySelector('#retry').addEventListener('click', () => window.__retryClicks++);");
    await driver.findElement(By.id('retry')).sendKeys(' ');
    assert.equal(await driver.executeScript('return window.__retryClicks'), 1, 'Keyboard activation retries exactly once');
    await driver.wait(retryReady, 10000);
    await writeFile(join(artifacts, 'startup-error-desktop.png'), await driver.takeScreenshot(), 'base64');
    await driver.manage().window().setRect({ width: 640, height: 520 });
    assert.equal(await driver.executeScript('return document.documentElement.scrollWidth <= innerWidth'), true);
    assert.notEqual(await driver.executeScript('return getComputedStyle(document.documentElement).scrollbarColor'), 'auto');
    await writeFile(join(artifacts, 'startup-error.png'), await driver.takeScreenshot(), 'base64');
    console.log(JSON.stringify({ failureRecovery: 'passed', keyboardRetry: 'passed', narrowViewport: 'passed', scrollbar: 'passed' }));
  } else {
    const workspaceVisible = async () => {
      try {
        return await driver.executeScript("return location.hostname === '127.0.0.1' && document.title.includes('DeepSeek Harness') && !document.querySelector('#heading') && document.body.innerText.length > 100 && !document.body.innerText.includes('authentication required')");
      } catch { return false; } // The startup handoff replaces the document.
    };
    await driver.wait(workspaceVisible, 90000);
    const visibleMs = Math.round(performance.now() - started);
    await delay(1500);
    await writeFile(join(artifacts, 'workspace.png'), await driver.takeScreenshot(), 'base64');
    const text = await driver.findElement(By.css('body')).getText();
    assert.ok(text.length > 100 && !text.includes('authentication required'), 'The actual workspace rendered');
    const title = await driver.getTitle();
    const denied = await driver.executeAsyncScript(`const done = arguments[arguments.length - 1]; if (!window.__TAURI__?.core?.invoke) done('denied'); else window.__TAURI__.core.invoke('start_backend').then(() => done('allowed')).catch(() => done('denied'));`);
    assert.equal(denied, 'denied', 'The hosted workspace has no desktop startup privileges');
    let metrics = [];
    try { metrics = (await readFile(join(data, 'local.deepseek.harness-linux/performance.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse); } catch {}
    const rows = execFileSync('ps', ['-eo', 'pid=,ppid=,rss=,comm='], { encoding: 'utf8' }).trim().split('\n').map(line => {
      const [pid, ppid, rss, comm] = line.trim().split(/\s+/); return { pid: Number(pid), ppid: Number(ppid), rss: Number(rss), comm };
    });
    const owned = new Set(appPid ? [appPid] : []);
    for (let i = 0; i < 8; i++) for (const row of rows) if (owned.has(row.ppid)) owned.add(row.pid);
    let pssKiB = 0;
    for (const pid of owned) { try { pssKiB += Number((await readFile(`/proc/${pid}/smaps_rollup`, 'utf8')).match(/^Pss:\s+(\d+)/m)?.[1] || 0); } catch {} }
    const report = { title, visibleMs, bodyCharacters: text.length, nativeAccessFromWorkspace: denied, metrics, processCount: owned.size, rssMiB: Math.round(rows.filter(row => owned.has(row.pid)).reduce((sum, row) => sum + row.rss, 0) / 1024), pssMiB: Math.round(pssKiB / 1024), note: 'Headless Xvfb, empty profile, no API request; RSS double-counts shared pages. Not an Electron comparison.' };
    await writeFile(join(artifacts, 'desktop-smoke.json'), `${JSON.stringify(report, null, 2)}\n`);
    console.log(JSON.stringify(report));
    const latestBackendPid = async () => {
      const lifecycle = await readFile(join(data, 'local.deepseek.harness-linux/desktop.log'), 'utf8');
      const matches = [...lifecycle.matchAll(/started \(pid (\d+)\)/g)];
      const pid = Number(matches.at(-1)?.[1]);
      assert.ok(pid, 'The isolated test app must have started a workspace process');
      return pid;
    };
    const processRunning = async pid => {
      try {
        const status = await readFile(`/proc/${pid}/status`, 'utf8');
        return !/^State:\s+Z/m.test(status);
      } catch { return false; }
    };
    const requireOwnedProcess = async pid => {
      const status = await readFile(`/proc/${pid}/status`, 'utf8');
      assert.equal(Number(status.match(/^PPid:\s+(\d+)/m)?.[1]), appPid, 'Only a child of the isolated test app may be stopped');
    };
    if (process.env.HARNESS_TEST_REOPEN === '1') {
      let nativeScreenshot = false;
      // Read only the session-bus address, keep it in memory, and never emit the
      // environment. The second launch must reach this test's resident instance.
      const appEnvironment = await readFile(`/proc/${appPid}/environ`, 'utf8');
      const busEntry = appEnvironment.split('\0').find(entry => entry.startsWith('DBUS_SESSION_BUS_ADDRESS='));
      assert.ok(busEntry, 'The isolated test app must have its own session bus');
      const reopenEnvironment = { ...env, DBUS_SESSION_BUS_ADDRESS: busEntry.slice('DBUS_SESSION_BUS_ADDRESS='.length) };
      for (const key of ['TAURI_AUTOMATION', 'TAURI_WEBVIEW_AUTOMATION', 'WEBKIT_INSPECTOR_SERVER']) delete reopenEnvironment[key];
      const reopen = async () => {
        const secondary = spawn(application, [], { env: reopenEnvironment, stdio: 'ignore' });
        await new Promise((resolve, reject) => {
          const timer = setTimeout(() => {
            secondary.kill('SIGTERM');
            reject(new Error('The secondary launch did not hand off within five seconds'));
          }, 5000);
          secondary.once('error', error => { clearTimeout(timer); reject(error); });
          secondary.once('exit', (code, signal) => {
            clearTimeout(timer);
            if (code === 0) resolve();
            else reject(new Error(`The secondary launch failed (${signal || code})`));
          });
        });
      };
      const originalBackendPid = await latestBackendPid();
      await requireOwnedProcess(originalBackendPid);
      await driver.executeScript(`
        const fixture = document.createElement('textarea');
        fixture.dataset.testid = 'reopen-draft-fixture';
        fixture.value = 'Draft retained across a healthy reopen';
        document.querySelector('#root').append(fixture);
      `);
      await reopen();
      // Reopening probes asynchronously; outwait its bounded evaluation so a
      // delayed, unconditional reload cannot make this assertion pass early.
      await delay(3500);
      assert.equal(await driver.executeScript("return document.querySelector('[data-testid=\"reopen-draft-fixture\"]')?.value"), 'Draft retained across a healthy reopen', 'A healthy reopen must preserve the current document and draft');
      assert.equal(await latestBackendPid(), originalBackendPid, 'A healthy reopen must preserve its workspace process');
      // Simulate a boot screen left behind after its initial loading grace.
      await delay(12000);
      await driver.executeScript(`
        const boot = document.createElement('div');
        boot.dataset.dshBoot = '';
        boot.textContent = 'Loading';
        document.querySelector('#root').replaceChildren(boot);
      `);
      await reopen();
      await driver.wait(async () => {
        try {
          return await workspaceVisible() && await driver.executeScript("return !document.querySelector('[data-dsh-boot]')");
        } catch { return false; }
      }, 15000, 'Reopening must recover a stale loading screen');
      assert.equal(await latestBackendPid(), originalBackendPid, 'Loading recovery must preserve the running workspace');
      await driver.executeScript('document.body.replaceChildren();');
      assert.equal(await workspaceVisible(), false, 'The renderer is blank before reopening');
      await writeFile(join(artifacts, 'reopen-blank.png'), await driver.takeScreenshot(), 'base64');
      await reopen();
      await driver.wait(workspaceVisible, 15000, 'Reopening must recover a blank renderer');
      assert.equal(await latestBackendPid(), originalBackendPid, 'Renderer recovery must preserve the running workspace');
      assert.equal(await processRunning(originalBackendPid), true);
      const reopenReport = { healthyDraftPreserved: true, staleLoadingRecovery: 'passed', blankRendererRecovery: 'passed', backendPreserved: true };
      console.log(JSON.stringify(reopenReport));

      await requireOwnedProcess(originalBackendPid);
      process.kill(originalBackendPid, 'SIGTERM');
      await driver.wait(async () => {
        try {
          return !(await processRunning(originalBackendPid)) && await driver.executeScript("return location.hostname !== '127.0.0.1' && new URLSearchParams(location.search).has('recovery')");
        } catch { return false; }
      }, 15000, 'The stopped workspace must reach its recovery screen');
      await reopen();
      await driver.wait(workspaceVisible, 15000, 'Reopening must restart a stopped workspace');
      const restartedBackendPid = await latestBackendPid();
      assert.notEqual(restartedBackendPid, originalBackendPid, 'A stopped workspace needs a new backend process');
      await requireOwnedProcess(restartedBackendPid);
      reopenReport.stoppedBackendRecovery = 'passed';
      reopenReport.newBackendStarted = true;
      console.log(JSON.stringify(reopenReport));

      // Also cover reopening while the native process monitor is still moving
      // from the stopped workspace to the local recovery page.
      const previousWorkspaceOrigin = await driver.executeScript('return location.origin');
      process.kill(restartedBackendPid, 'SIGTERM');
      await driver.wait(async () => !(await processRunning(restartedBackendPid)), 5000,
        'The backend must have exited before reopening');
      await reopen();
      await driver.wait(async () => {
        try {
          return await workspaceVisible()
            && await driver.executeScript('return location.origin') !== previousWorkspaceOrigin
            && await latestBackendPid() !== restartedBackendPid;
        } catch { return false; }
      }, 15000, 'An immediate reopen must survive the backend recovery transition');
      const activeBackendPid = await latestBackendPid();
      await requireOwnedProcess(activeBackendPid);
      reopenReport.immediateReopenRecovery = 'passed';

      if (process.env.HARNESS_TEST_WEBKIT_CRASH === '1') {
        const readinessEvents = async () => (await readFile(join(data, 'local.deepseek.harness-linux/performance.jsonl'), 'utf8'))
          .trim().split('\n').map(JSON.parse);
        // A URL can change before its new document finishes mounting. Isolate
        // renderer recovery from the preceding backend restart by requiring
        // the native app to have observed this backend's usable workspace.
        await driver.wait(async () => {
          try {
            const events = await readinessEvents();
            return events.findLastIndex(event => event.phase === 'workspace_ready')
              > events.findLastIndex(event => event.phase === 'backend_ready');
          } catch { return false; }
        }, 15000, 'The replacement backend workspace must mount before the renderer crash test');
        const readyCount = async () => (await readinessEvents()).filter(event => event.phase === 'workspace_ready').length;
        const beforeCrashReadyCount = await readyCount();
        const childRows = execFileSync('ps', ['-eo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).trim().split('\n');
        const rendererPid = childRows.map(line => line.trim().split(/\s+/)).find(([, parent, comm]) => Number(parent) === appPid && comm.startsWith('WebKitWebProces'))?.[0];
        assert.ok(rendererPid, 'The test app must own its WebKit renderer');
        await requireOwnedProcess(Number(rendererPid));
        process.kill(Number(rendererPid), 'SIGKILL');
        await reopen();
        // WebKitWebDriver remains attached to the terminated page process.
        // Verify that the native app observes the replacement view mounted,
        // then capture the actual display rather than that stale automation view.
        await driver.wait(async () => await readyCount() > beforeCrashReadyCount, 15000,
          'Reopening must mount a replacement WebKit renderer');
        assert.equal(await latestBackendPid(), activeBackendPid, 'A renderer crash must preserve the workspace process');
        const replacementRows = execFileSync('ps', ['-eo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).trim().split('\n');
        assert.ok(replacementRows.some(line => {
          const [pid, parent, comm] = line.trim().split(/\s+/);
          return Number(parent) === appPid && comm.startsWith('WebKitWebProces') && pid !== rendererPid;
        }), 'A replacement renderer process must belong to the same native app');
        execFileSync('python3', ['-c', `
import gi, sys
gi.require_version('Gdk', '3.0')
from gi.repository import Gdk
Gdk.init([])
root = Gdk.get_default_root_window()
image = Gdk.pixbuf_get_from_window(root, 0, 0, root.get_width(), root.get_height())
image.savev(sys.argv[1], 'png', [], [])
`, join(artifacts, 'reopen-recovered.png')], { env, stdio: 'ignore' });
        nativeScreenshot = true;
        reopenReport.webKitCrashRecovery = 'passed';
      }
      if (!nativeScreenshot) await writeFile(join(artifacts, 'reopen-recovered.png'), await driver.takeScreenshot(), 'base64');
      await writeFile(join(artifacts, 'desktop-reopen.json'), `${JSON.stringify(reopenReport, null, 2)}\n`);
      console.log(JSON.stringify(reopenReport));
    }
    if (process.env.HARNESS_TEST_CRASH === '1') {
      const backendPid = await latestBackendPid();
      await requireOwnedProcess(backendPid);
      process.kill(backendPid, 'SIGKILL');
      await driver.wait(async () => (await driver.getCurrentUrl()).includes('recovery=1'), 15000);
      await driver.wait(async () => {
        try { return await driver.executeScript("const retry=document.querySelector('#retry'); return Boolean(retry && !retry.hidden && !retry.disabled)"); }
        catch { return false; }
      }, 5000);
      await driver.findElement(By.id('retry')).click();
      await driver.wait(workspaceVisible, 60000);
      console.log(JSON.stringify({ crashRecovery: 'passed' }));
    }
    const backendPid = await latestBackendPid();
    await requireOwnedProcess(backendPid);
    for (const pid of [appPid, backendPid]) {
      const argumentsText = await readFile(`/proc/${pid}/cmdline`, 'utf8');
      assert.equal(hasLaunchCredential(argumentsText), false,
        'Desktop and backend arguments contain no authenticated startup URL');
    }
    for (const name of ['desktop.log', 'performance.jsonl']) {
      const log = await readFile(join(data, 'local.deepseek.harness-linux', name), 'utf8');
      assert.equal(hasLaunchCredential(log), false,
        'Desktop logs contain no authenticated startup URL');
    }
    assert.equal(launchCredentialPrinted, false, 'The desktop never forwards the private startup URL to stdout or stderr');
    console.log(JSON.stringify({ startupCredentialsPrivate: 'passed' }));
  }
} catch (error) {
  const diagnostics = {};
  for (const name of ['performance.jsonl', 'desktop.log']) {
    try {
      const log = await readFile(join(data, 'local.deepseek.harness-linux', name), 'utf8');
      diagnostics[name] = hasLaunchCredential(log) ? 'Omitted: startup credential detected in log.' : log;
    } catch {}
  }
  try {
    diagnostics.processes = execFileSync('ps', ['-eo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).trim().split('\n').filter(line => /harness-for|WebKit|tauri-driver/.test(line));
  } catch {}
  await writeFile(join(artifacts, 'desktop-failure.json'), `${JSON.stringify(diagnostics, null, 2)}\n`);
  if (driver) {
    try { await writeFile(join(artifacts, 'desktop-failure.png'), await driver.takeScreenshot(), 'base64'); } catch {}
  }
  throw error;
} finally {
  if (appPid) { try { process.kill(appPid, 'SIGTERM'); } catch {} }
  if (driver) { try { await driver.quit(); } catch {} }
  try { process.kill(-server.pid, 'SIGTERM'); } catch {}
  xvfb.kill('SIGTERM');
  await delay(500);
  await rm(scratch, { recursive: true, force: true });
}
