const message = document.querySelector('#message');
const detail = document.querySelector('#detail');
const retry = document.querySelector('#retry');
let starting = false;

async function start() {
  if (starting) return;
  starting = true;
  retry.disabled = true;
  document.body.dataset.state = 'loading';
  document.title = 'Opening Harness — DeepSeek Harness for Linux';
  message.textContent = 'Opening your workspace…';
  detail.textContent = 'Your workspace settings are saved on this computer.';
  try {
    if (!window.__TAURI__?.core?.invoke) {
      throw new Error('Open DeepSeek Harness for Linux from your applications menu. This page needs the desktop app.');
    }
    await window.__TAURI__.core.invoke('start_backend');
    message.textContent = 'Your workspace is ready.';
    await window.__TAURI__.core.invoke('open_workspace');
  } catch (error) {
    document.body.dataset.state = 'error';
    document.title = 'Unable to open Harness — DeepSeek Harness for Linux';
    message.textContent = 'Your workspace couldn’t open.';
    detail.textContent = String(error);
    retry.hidden = false;
    retry.disabled = false;
    retry.focus();
  } finally {
    starting = false;
  }
}
retry.addEventListener('click', start);
window.addEventListener('harness-reopen', () => {
  if (!starting) start();
});
const recovery = new URLSearchParams(window.location.search).get('recovery');
if (recovery) {
  document.body.dataset.state = 'error';
  document.title = 'Workspace stopped — DeepSeek Harness for Linux';
  message.textContent = recovery === 'frontend'
    ? 'Your workspace couldn’t finish opening.'
    : 'Your workspace stopped unexpectedly.';
  detail.textContent = 'Try again to reopen your saved workspace.';
  retry.hidden = false;
  retry.disabled = false;
  retry.focus();
} else {
  start();
}
