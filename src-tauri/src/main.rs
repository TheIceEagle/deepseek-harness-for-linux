#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod recovery;

use backend::Backend;
use recovery::show_workspace;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, State, WebviewWindow, WebviewWindowBuilder};
use url::{Host, Url};

struct AppState {
    backend: Arc<Mutex<Backend>>,
    workspace_status: MenuItem<tauri::Wry>,
    ready_url: Mutex<Option<Url>>,
    port: AtomicU16,
    rendered: AtomicBool,
    auth_retried: AtomicBool,
    document_loaded: AtomicBool,
    load_started: Mutex<Instant>,
    view_generation: AtomicU64,
    view_probe_running: AtomicBool,
    reopen_pending: AtomicBool,
    reopen_requested: AtomicBool,
    exiting: AtomicBool,
    shutdown: Arc<AtomicBool>,
    launched: Instant,
    data_dir: PathBuf,
}

impl AppState {
    fn set_workspace_running(&self, running: bool) {
        let _ = self.workspace_status.set_text(if running {
            "Workspace running"
        } else {
            "Workspace stopped"
        });
    }
}

#[derive(Serialize)]
struct StartupInfo {
    startup_ms: u64,
}

fn bundled_page(url: &Url) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost")))
}

fn external_link(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|address| address.is_loopback())
        }
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            host == "localhost" || host.ends_with(".localhost")
        }
        None => return false,
    };
    // A popup or an in-flight navigation from a stopped backend must not send
    // its private launch credential to xdg-open or a browser's process arguments.
    // Keep ordinary local tool links available without their sign-in URLs.
    !(loopback
        && url
            .query_pairs()
            .any(|(name, _)| name.eq_ignore_ascii_case("token")))
}

fn require_startup_page(window: &WebviewWindow) -> Result<(), String> {
    if bundled_page(&window.url().map_err(|_| "Could not verify the app page.")?) {
        Ok(())
    } else {
        Err("Desktop startup commands are only available from the bundled startup page.".into())
    }
}

fn metric(state: &AppState, phase: &str, milliseconds: u128) {
    // Only timing and phase are recorded. URLs, tokens and conversation data are excluded.
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.data_dir.join("performance.jsonl"))
    {
        let _ = writeln!(
            file,
            "{{\"phase\":\"{phase}\",\"milliseconds\":{milliseconds}}}"
        );
    }
    eprintln!("Harness: {phase} in {milliseconds} ms");
}

#[tauri::command]
async fn start_backend(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<StartupInfo, String> {
    require_startup_page(&window)?;
    let backend = Arc::clone(&state.backend);
    let result = tauri::async_runtime::spawn_blocking(move || {
        backend
            .lock()
            .map_err(|_| "The workspace process is unavailable. Restart the app.".to_string())?
            .start()
    })
    .await
    .map_err(|_| "The workspace process could not start. Try again.".to_string())
    .and_then(|result| result);
    let ready = match result {
        Ok(ready) => ready,
        Err(error) => {
            state.set_workspace_running(false);
            return Err(error);
        }
    };
    let url = Url::parse(&ready.url).map_err(|_| "The workspace returned an invalid address.")?;
    state.port.store(
        url.port()
            .ok_or("The workspace returned an invalid port.")?,
        Ordering::SeqCst,
    );
    *state
        .ready_url
        .lock()
        .map_err(|_| "The workspace address is unavailable.")? = Some(url);
    recovery::prepare_workspace(&state);
    state.set_workspace_running(true);
    metric(&state, "backend_ready", ready.startup_ms.into());
    Ok(StartupInfo {
        startup_ms: ready.startup_ms,
    })
}

#[tauri::command]
fn open_workspace(window: WebviewWindow, state: State<'_, AppState>) -> Result<(), String> {
    require_startup_page(&window)?;
    let url = state
        .ready_url
        .lock()
        .map_err(|_| "The workspace address is unavailable.")?
        .clone()
        .ok_or("Start the workspace before opening it.")?;
    window
        .navigate(url)
        .map_err(|_| "The workspace could not open. Try again.".into())
}

fn main() {
    let launched = Instant::now();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_workspace(app)
        }))
        .invoke_handler(tauri::generate_handler![start_backend, open_workspace])
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let mut runtime_dir = app.path().resource_dir()?.join("runtime");
            if cfg!(debug_assertions) {
                runtime_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../runtime");
                if let Some(override_dir) = std::env::var_os("HARNESS_TEST_RUNTIME_DIR") {
                    runtime_dir = PathBuf::from(override_dir);
                }
            }
            let backend = Backend::new(runtime_dir, data_dir.clone());
            let shutdown = backend.shutdown_signal();
            let workspace_status = MenuItem::with_id(
                app,
                "workspace-status",
                "Workspace stopped",
                false,
                None::<&str>,
            )?;
            app.manage(AppState {
                backend: Arc::new(Mutex::new(backend)),
                workspace_status: workspace_status.clone(),
                ready_url: Mutex::new(None),
                port: AtomicU16::new(0),
                rendered: AtomicBool::new(false),
                auth_retried: AtomicBool::new(false),
                document_loaded: AtomicBool::new(false),
                load_started: Mutex::new(Instant::now()),
                view_generation: AtomicU64::new(0),
                view_probe_running: AtomicBool::new(false),
                reopen_pending: AtomicBool::new(false),
                reopen_requested: AtomicBool::new(false),
                exiting: AtomicBool::new(false),
                shutdown,
                launched,
                data_dir,
            });
            let navigation_app = app.handle().clone();
            WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
                .on_navigation(move |url| {
                    let state = navigation_app.state::<AppState>();
                    if bundled_page(url)
                        || (url.scheme() == "http"
                            && url.host_str() == Some("127.0.0.1")
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.port() == Some(state.port.load(Ordering::SeqCst)))
                    {
                        return true;
                    }
                    if external_link(url) {
                        let _ = open::that_detached(url.as_str());
                    }
                    false
                })
                .on_new_window(|url, _| {
                    if external_link(&url) {
                        let _ = open::that_detached(url.as_str());
                    }
                    tauri::webview::NewWindowResponse::Deny
                })
                .build()?;
            let show = MenuItem::with_id(app, "show", "Open DeepSeek Harness", true, None::<&str>)?;
            let reload = MenuItem::with_id(app, "reload", "Reload workspace", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit DeepSeek Harness", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&workspace_status, &show, &reload, &quit])?;
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .ok_or("The app icon is missing.")?
                        .clone(),
                )
                .tooltip("DeepSeek Harness for Linux")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_workspace(app),
                    "reload" => recovery::reload_workspace(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_workspace(tray.app_handle());
                    }
                })
                .build(app)?;
            let monitor_app = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let state = monitor_app.state::<AppState>();
                if state.exiting.load(Ordering::SeqCst) {
                    break;
                }
                if state.port.load(Ordering::SeqCst) == 0 {
                    continue;
                }
                // Keep the process lock through the recovery transition. A
                // reopen must not start a replacement which this old liveness
                // result would then overwrite with the stopped-process page.
                let Ok(mut backend) = state.backend.lock() else {
                    continue;
                };
                let running = backend.is_running();
                if !running && !state.exiting.load(Ordering::SeqCst) {
                    state.set_workspace_running(false);
                    state.port.store(0, Ordering::SeqCst);
                    state.view_generation.fetch_add(1, Ordering::SeqCst);
                    state.rendered.store(false, Ordering::SeqCst);
                    if let Ok(mut ready) = state.ready_url.lock() {
                        *ready = None;
                    }
                    if let Some(window) = monitor_app.get_webview_window("main") {
                        let _ = window.navigate(
                            Url::parse("tauri://localhost/index.html?recovery=1")
                                .expect("static app URL"),
                        );
                    }
                } else if !state.rendered.load(Ordering::SeqCst) {
                    recovery::probe_workspace(&monitor_app, false);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .on_page_load(|window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                let state = window.state::<AppState>();
                if bundled_page(payload.url())
                    && state.reopen_requested.load(Ordering::SeqCst)
                {
                    // A reopen can arrive after the monitor queued this page,
                    // before its event listener existed. Replay that request
                    // after the bundled document has finished loading.
                    let _ = window.eval("window.dispatchEvent(new Event('harness-reopen'))");
                }
                if payload.url().host_str() == Some("127.0.0.1")
                    && payload.url().port() == Some(state.port.load(Ordering::SeqCst))
                {
                    let loaded = window.clone();
                    let _ = window.eval_with_callback(
                        "Boolean(document.body && document.body.innerText.includes('dsh web authentication required;'))",
                        move |result| {
                            let state = loaded.state::<AppState>();
                            if result.trim() == "true" {
                                // WebKit withholds a Strict cookie on a redirect chain
                                // initiated at tauri://localhost. A same-origin reload
                                // uses it after that document has committed. Keep the
                                // upstream cookie policy and bound this to one retry.
                                if !state.auth_retried.swap(true, Ordering::SeqCst) {
                                    let _ = loaded.eval("window.location.reload()");
                                } else {
                                    state.reopen_requested.store(false, Ordering::SeqCst);
                                    let _ = loaded.navigate(Url::parse("tauri://localhost/index.html?recovery=auth").expect("static app URL"));
                                }
                            } else {
                                if !state.document_loaded.swap(true, Ordering::SeqCst) {
                                    metric(&state, "workspace_document_loaded", state.launched.elapsed().as_millis());
                                }
                                recovery::probe_workspace(loaded.app_handle(), false);
                            }
                        },
                    );
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Could not start DeepSeek Harness for Linux");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<AppState>();
            if !state.exiting.swap(true, Ordering::SeqCst) {
                state.shutdown.store(true, Ordering::SeqCst);
                api.prevent_exit();
                let backend = Arc::clone(&state.backend);
                let handle = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Ok(mut backend) = backend.lock() {
                        backend.stop();
                    }
                    handle.exit(0);
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_launch_links_never_reach_external_processes() {
        for candidate in [
            "http://127.0.0.1:3081/?token=fixture",
            "http://127.0.0.1:3999/?token=stale-fixture",
            "http://127.1:3081/?%74oken=encoded-fixture",
            "http://2130706433:3081/?TOKEN=fixture",
            "http://localhost:3081/?token=fixture",
            "http://localhost.:3081/?token=fixture",
            "https://app.localhost:3081/path?next=1&token=fixture",
            "http://[::1]:3081/?token=fixture",
            "http://[::ffff:127.0.0.1]:3081/?token=fixture",
            "https://127.0.0.1:3081/?token=fixture",
            "http://user:password@example.com/",
            "https://%75ser@example.com/",
            "file:///tmp/workspace",
            "javascript:alert(1)",
        ] {
            assert!(!external_link(&Url::parse(candidate).unwrap()));
        }
    }

    #[test]
    fn ordinary_external_and_local_tool_links_still_open() {
        for candidate in [
            "https://www.deepseek.com/",
            "https://example.com/login?token=service-owned-token",
            "http://127.0.0.1:8000/docs",
            "http://localhost:8080/?page=tools",
            "http://[::1]:8080/",
        ] {
            assert!(external_link(&Url::parse(candidate).unwrap()));
        }
    }
}
