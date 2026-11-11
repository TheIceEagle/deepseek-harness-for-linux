mod dialogs;
mod markdown;
mod messages;
mod tools;
mod tools_ui;
mod ui;

use gtk::{gio, glib, prelude::*};
use harness_native_core::Store;
use std::{cell::RefCell, path::PathBuf, rc::Rc};

fn profile_directory() -> Result<PathBuf, &'static str> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("The native app cannot locate your private data folder.")?;
    Ok(base.join("deepseek-harness-native"))
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("io.github.theiceeagle.DeepSeekHarnessNative")
        .flags(gio::ApplicationFlags::empty())
        .build();
    let current: Rc<RefCell<Option<Rc<ui::Ui>>>> = Rc::default();
    let active = current.clone();
    app.connect_activate(move |app| {
        if let Some(ui) = active.borrow().as_ref() {
            ui.window.present();
            return;
        }
        if let Some(window) = app.windows().first() {
            window.present();
            return;
        }
        let result = profile_directory()
            .map_err(str::to_owned)
            .and_then(|directory| {
                let store = Store::open(&directory).map_err(|error| error.to_string())?;
                ui::Ui::build(app, store, directory)
            });
        match result {
            Ok(ui) => {
                ui.window.present();
                *active.borrow_mut() = Some(ui);
            }
            Err(message) => ui::startup_error(app, &message),
        }
    });
    app.connect_shutdown(move |_| {
        if let Some(ui) = current.borrow_mut().take() {
            ui.shutdown();
        }
    });
    app.run()
}
