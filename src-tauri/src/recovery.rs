//! Recover the desktop view without interrupting a healthy Harness process.

use super::{bundled_page, metric, AppState};
use std::{
    sync::{atomic::Ordering, mpsc},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewWindow};
use url::Url;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const REOPEN_LOADING_GRACE: Duration = Duration::from_secs(15);
const FRONTEND_DEADLINE: Duration = Duration::from_secs(45);

const VIEW_PROBE: &str = r#"(() => {
  if (location.protocol === 'tauri:' ||
      (location.protocol === 'http:' && location.hostname === 'tauri.localhost')) return 'startup';
  const root = document.querySelector('#root');
  if (document.readyState !== 'complete' ||
      (root && (root.matches('[data-dsh-boot]') || root.querySelector('[data-dsh-boot]')))) return 'loading';
  if (document.body && document.body.innerText.includes('dsh web authentication required;')) return 'auth';
  if (root && root.childElementCount > 0 &&
      root.querySelector('button, textarea, nav, [contenteditable="true"]')) return 'ready';
  return 'blank';
})()"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewStatus {
    Startup,
    Loading,
    Auth,
    Ready,
    Blank,
    Unresponsive,
}

fn decode_status(result: &str) -> ViewStatus {
    match result.trim().trim_matches('"') {
        "startup" => ViewStatus::Startup,
        "loading" => ViewStatus::Loading,
        "auth" => ViewStatus::Auth,
        "ready" => ViewStatus::Ready,
        "blank" => ViewStatus::Blank,
        _ => ViewStatus::Unresponsive,
    }
}

pub fn prepare_workspace(state: &AppState) {
    state.view_generation.fetch_add(1, Ordering::SeqCst);
    state.auth_retried.store(false, Ordering::SeqCst);
    state.rendered.store(false, Ordering::SeqCst);
    state.document_loaded.store(false, Ordering::SeqCst);
    if let Ok(mut started) = state.load_started.lock() {
        *started = Instant::now();
    }
}

pub fn show_workspace(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Keep explicit reopen intent until a usable workspace is observed. In
    // particular, the monitor may be midway through replacing a dead remote
    // document with the bundled recovery page.
    state.reopen_requested.store(true, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        if window.url().is_ok_and(|url| bundled_page(&url)) {
            // An old remote probe may still own the probe guard for up to
            // three seconds. It must not swallow reopening a recovery page.
            dispatch_reopen(&window);
            return;
        }
    }
    probe_workspace(app, true);
}

pub fn reload_workspace(app: &AppHandle) {
    if app.try_state::<AppState>().is_none() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        if state.exiting.load(Ordering::SeqCst) {
            return;
        }
        let generation = state.view_generation.load(Ordering::SeqCst);
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        // A startup command holds this mutex while it boots. Never wait for it
        // from the desktop action or create a competing startup attempt.
        let running = match state.backend.try_lock() {
            Ok(mut backend) => backend.is_running(),
            Err(_) => {
                if window.url().is_ok_and(|url| bundled_page(&url)) {
                    dispatch_reopen(&window);
                }
                return;
            }
        };
        if !current(&state, generation) {
            return;
        }
        if running {
            reload_view(&window, &state, generation);
        } else {
            open_startup(&window, &state, generation);
        }
    });
}

pub fn probe_workspace(app: &AppHandle, reopening: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state.exiting.load(Ordering::SeqCst) {
        return;
    }
    // Set this before the guard check: a finishing probe cannot miss a reopen
    // request arriving between its guard release and pending-request exchange.
    if reopening {
        state.reopen_pending.store(true, Ordering::SeqCst);
    }
    if state
        .view_probe_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    if reopening {
        state.reopen_pending.store(false, Ordering::SeqCst);
    }
    let generation = state.view_generation.load(Ordering::SeqCst);
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = ProbeGuard(app.clone());
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        if !current(&state, generation) {
            return;
        }
        if window.url().is_ok_and(|url| bundled_page(&url)) {
            if reopening {
                dispatch_reopen(&window);
            }
            return;
        }
        if !window.url().is_ok_and(|url| {
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port() == Some(state.port.load(Ordering::SeqCst))
        }) {
            // WebKit can lose its current URI when its renderer terminates.
            // An explicit reopen can still restore our cached, trusted launch
            // URL; never probe an unknown document with JavaScript.
            if reopening
                && state.port.load(Ordering::SeqCst) != 0
                && state.ready_url.lock().is_ok_and(|ready| ready.is_some())
            {
                reload_workspace(&app);
            }
            return;
        }
        if reopening {
            let running = match state.backend.try_lock() {
                Ok(mut backend) => backend.is_running(),
                Err(_) => return,
            };
            if !current(&state, generation) {
                return;
            }
            if !running {
                open_startup(&window, &state, generation);
                return;
            }
        }

        let (sender, receiver) = mpsc::sync_channel(1);
        let status = if window
            .eval_with_callback(VIEW_PROBE, move |result| {
                let _ = sender.send(decode_status(&result));
            })
            .is_ok()
        {
            receiver
                .recv_timeout(PROBE_TIMEOUT)
                .unwrap_or(ViewStatus::Unresponsive)
        } else {
            ViewStatus::Unresponsive
        };
        if !current(&state, generation) {
            return;
        }
        let elapsed = state
            .load_started
            .lock()
            .map(|started| started.elapsed())
            .unwrap_or(FRONTEND_DEADLINE);
        match status {
            ViewStatus::Startup => {
                if reopening {
                    dispatch_reopen(&window);
                }
            }
            ViewStatus::Ready => {
                state.reopen_requested.store(false, Ordering::SeqCst);
                state.document_loaded.store(true, Ordering::SeqCst);
                if !state.rendered.swap(true, Ordering::SeqCst) {
                    metric(
                        &state,
                        "workspace_ready",
                        state.launched.elapsed().as_millis(),
                    );
                }
            }
            _ if reopening
                && (status != ViewStatus::Loading || elapsed >= REOPEN_LOADING_GRACE) =>
            {
                reload_view(&window, &state, generation);
            }
            _ if !reopening && elapsed >= FRONTEND_DEADLINE => {
                // Keep Node and any active tasks running. The local page offers
                // a visible retry rather than leaving an endless loading view.
                if current(&state, generation) {
                    state.reopen_requested.store(false, Ordering::SeqCst);
                    prepare_workspace(&state);
                    let _ = window.navigate(
                        Url::parse("tauri://localhost/index.html?recovery=frontend")
                            .expect("static recovery URL"),
                    );
                }
            }
            _ => {}
        }
    });
}

fn current(state: &AppState, generation: u64) -> bool {
    !state.exiting.load(Ordering::SeqCst)
        && state.view_generation.load(Ordering::SeqCst) == generation
}

fn dispatch_reopen(window: &WebviewWindow) {
    let _ = window.eval("window.dispatchEvent(new Event('harness-reopen'))");
}

fn reload_view(window: &WebviewWindow, state: &AppState, generation: u64) {
    let url = state.ready_url.lock().ok().and_then(|ready| ready.clone());
    if !current(state, generation) {
        return;
    }
    if let Some(url) = url {
        metric(
            state,
            "workspace_reloading",
            state.launched.elapsed().as_millis(),
        );
        prepare_workspace(state);
        let _ = window.navigate(url);
    } else {
        open_startup(window, state, generation);
    }
}

fn open_startup(window: &WebviewWindow, state: &AppState, generation: u64) {
    if !current(state, generation) {
        return;
    }
    state.port.store(0, Ordering::SeqCst);
    if let Ok(mut ready) = state.ready_url.lock() {
        *ready = None;
    }
    prepare_workspace(state);
    let _ = window
        .navigate(Url::parse("tauri://localhost/index.html?resume=1").expect("static startup URL"));
}

struct ProbeGuard(AppHandle);

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let Some(state) = self.0.try_state::<AppState>() else {
            return;
        };
        state.view_probe_running.store(false, Ordering::SeqCst);
        let reopen = state.reopen_pending.swap(false, Ordering::SeqCst)
            && !state.exiting.load(Ordering::SeqCst);
        if reopen {
            probe_workspace(&self.0, true);
        }
    }
}
