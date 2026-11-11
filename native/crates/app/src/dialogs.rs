//! Modal windows built from current GTK primitives, without deprecated Dialogs.

use gtk::{gdk, gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

type ResponseHandler = Rc<dyn Fn(&Dialog, gtk::ResponseType)>;

struct Responses {
    handlers: RefCell<Vec<ResponseHandler>>,
    buttons: RefCell<Vec<(gtk::ResponseType, glib::WeakRef<gtk::Button>)>>,
    default: Cell<gtk::ResponseType>,
    responding: Cell<bool>,
    closed: Cell<bool>,
    parent: glib::WeakRef<gtk::Window>,
    focus: glib::WeakRef<gtk::Widget>,
}

impl Default for Responses {
    fn default() -> Self {
        Self {
            handlers: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
            default: Cell::new(gtk::ResponseType::None),
            responding: Cell::new(false),
            closed: Cell::new(false),
            parent: glib::WeakRef::new(),
            focus: glib::WeakRef::new(),
        }
    }
}

#[derive(Clone)]
pub struct Dialog {
    window: gtk::Window,
    content: gtk::Box,
    actions: gtk::Box,
    responses: Rc<Responses>,
}

impl Dialog {
    pub fn new(title: &str, parent: &impl IsA<gtk::Window>, width: i32) -> Self {
        let parent = parent.as_ref();
        let height = parent
            .surface()
            .and_then(|surface| surface.display().monitor_at_surface(&surface))
            .map(|monitor| (monitor.geometry().height() - 120).clamp(280, 600))
            .unwrap_or(520);
        let window = gtk::Window::builder()
            .title(title)
            .transient_for(parent)
            .modal(true)
            .destroy_with_parent(true)
            .default_width(width)
            .default_height(height)
            .resizable(true)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .build();
        window.set_application(parent.application().as_ref());
        window.update_property(&[gtk::accessible::Property::Label(title)]);
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        actions.set_margin_start(24);
        actions.set_margin_end(24);
        actions.set_margin_top(12);
        actions.set_margin_bottom(20);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(160)
            .max_content_height(480)
            .propagate_natural_height(true)
            .vexpand(true)
            .build();
        layout.append(&scroll);
        layout.append(&actions);
        window.set_child(Some(&layout));
        let responses = Rc::new(Responses::default());
        responses.parent.set(Some(parent));
        responses
            .focus
            .set(gtk::prelude::RootExt::focus(parent).as_ref());
        let dialog = Self {
            window,
            content,
            actions,
            responses,
        };

        let weak = dialog.downgrade();
        dialog.window.connect_close_request(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.respond(gtk::ResponseType::DeleteEvent);
                dialog.close();
            }
            glib::Propagation::Stop
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = dialog.downgrade();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(dialog) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::Escape {
                let response = if dialog.has_response(gtk::ResponseType::Cancel) {
                    gtk::ResponseType::Cancel
                } else {
                    gtk::ResponseType::Close
                };
                dialog.respond(response);
                dialog.close();
                return glib::Propagation::Stop;
            }
            // Single-line fields activate the default action. Multiline fields
            // retain Enter, and a focused button keeps its own activation.
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && !modifiers.intersects(
                    gdk::ModifierType::SHIFT_MASK
                        | gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK
                        | gdk::ModifierType::SUPER_MASK,
                )
            {
                let uses_enter =
                    gtk::prelude::RootExt::focus(&dialog.window).is_some_and(|focus| {
                        focus.is::<gtk::TextView>()
                            || focus.ancestor(gtk::TextView::static_type()).is_some()
                            || focus.is::<gtk::Button>()
                            || focus.ancestor(gtk::Button::static_type()).is_some()
                    });
                if !uses_enter {
                    if let Some(button) = dialog.default_button() {
                        if button.is_sensitive() && button.is_visible() {
                            button.emit_clicked();
                            return glib::Propagation::Stop;
                        }
                    }
                }
            }
            glib::Propagation::Proceed
        });
        dialog.window.add_controller(keys);
        dialog
    }

    pub fn window(&self) -> &gtk::Window {
        &self.window
    }

    pub fn content_area(&self) -> gtk::Box {
        self.content.clone()
    }

    pub fn add_button(&self, label: &str, response: gtk::ResponseType) -> gtk::Button {
        let button = gtk::Button::with_label(label);
        button.set_size_request(-1, 36);
        if matches!(response, gtk::ResponseType::Accept | gtk::ResponseType::Ok) {
            button.add_css_class("suggested-action");
        }
        self.actions.append(&button);
        self.responses
            .buttons
            .borrow_mut()
            .push((response, button.downgrade()));
        let weak = self.downgrade();
        button.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.respond(response);
            }
        });
        if self.responses.default.get() == gtk::ResponseType::None
            && matches!(
                response,
                gtk::ResponseType::Accept | gtk::ResponseType::Ok | gtk::ResponseType::Close
            )
        {
            self.set_default_response(response);
        } else if self.responses.default.get() == response {
            self.window.set_default_widget(Some(&button));
        }
        button
    }

    pub fn connect_response(&self, callback: impl Fn(&Dialog, gtk::ResponseType) + 'static) {
        self.responses.handlers.borrow_mut().push(Rc::new(callback));
    }

    pub fn set_default_response(&self, response: gtk::ResponseType) {
        self.responses.default.set(response);
        self.window
            .set_default_widget(self.default_button().as_ref());
    }

    pub fn present(&self) {
        if !self.responses.closed.get() {
            self.window.present();
        }
    }

    pub fn close(&self) {
        if self.responses.closed.replace(true) {
            return;
        }
        // GTK 4.14 can retain these pointers until the window is finalized,
        // after the response buttons below have already been removed.
        self.window.set_default_widget(None::<&gtk::Widget>);
        gtk::prelude::RootExt::set_focus(&self.window, None::<&gtk::Widget>);
        // Call destroy rather than close, which would emit close-request again.
        self.window.destroy();
        self.responses.handlers.borrow_mut().clear();
        self.responses.buttons.borrow_mut().clear();
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        while let Some(child) = self.actions.first_child() {
            self.actions.remove(&child);
        }
        if let Some(parent) = self.responses.parent.upgrade() {
            if parent.is_visible() {
                parent.present();
                if let Some(focus) = self.responses.focus.upgrade() {
                    focus.grab_focus();
                }
            }
        }
    }

    fn respond(&self, response: gtk::ResponseType) {
        if self.responses.closed.get() || self.responses.responding.replace(true) {
            return;
        }
        let handlers = self.responses.handlers.borrow().clone();
        if handlers.is_empty() {
            self.close();
        } else {
            for callback in handlers {
                callback(self, response);
                if self.responses.closed.get() {
                    break;
                }
            }
        }
        self.responses.responding.set(false);
    }

    fn has_response(&self, response: gtk::ResponseType) -> bool {
        self.responses
            .buttons
            .borrow()
            .iter()
            .any(|(candidate, _)| *candidate == response)
    }

    fn default_button(&self) -> Option<gtk::Button> {
        self.responses
            .buttons
            .borrow()
            .iter()
            .find(|(response, _)| *response == self.responses.default.get())
            .and_then(|(_, button)| button.upgrade())
    }

    fn downgrade(&self) -> WeakDialog {
        WeakDialog {
            window: self.window.downgrade(),
            content: self.content.downgrade(),
            actions: self.actions.downgrade(),
            responses: self.responses.clone(),
        }
    }
}

#[derive(Clone)]
struct WeakDialog {
    window: glib::WeakRef<gtk::Window>,
    content: glib::WeakRef<gtk::Box>,
    actions: glib::WeakRef<gtk::Box>,
    responses: Rc<Responses>,
}

impl WeakDialog {
    fn upgrade(&self) -> Option<Dialog> {
        Some(Dialog {
            window: self.window.upgrade()?,
            content: self.content.upgrade()?,
            actions: self.actions.upgrade()?,
            responses: self.responses.clone(),
        })
    }
}

pub fn choose_folder(
    parent: &impl IsA<gtk::Window>,
    callback: impl FnOnce(Result<Option<PathBuf>, String>) + 'static,
) {
    let chooser = gtk::FileDialog::builder()
        .title("Choose an extension folder")
        .accept_label("Install extension")
        .modal(true)
        .build();
    chooser.select_folder(Some(parent), None::<&gio::Cancellable>, move |result| {
        callback(match result {
            Ok(file) => file
                .path()
                .map(Some)
                .ok_or_else(|| "Choose a local extension folder.".to_owned()),
            Err(error)
                if error.matches(gtk::DialogError::Dismissed)
                    || error.matches(gtk::DialogError::Cancelled)
                    || error.matches(gio::IOErrorEnum::Cancelled) =>
            {
                Ok(None)
            }
            Err(_) => Err("The extension folder chooser could not open. Try again.".to_owned()),
        });
    });
}
