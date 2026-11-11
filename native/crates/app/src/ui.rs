use crate::dialogs::{self, Dialog};
use crate::messages::Messages;
use crate::tools::{ToolHub, ToolSet};
use gtk::{gdk, glib, prelude::*};
use harness_native_core::{
    AgentEvent, AgentOutcome, CoreError, MessageStatus, Provider, Role, Settings, Store,
    StreamOutcome, ToolCall, ToolDecision, ToolRound,
};
use harness_native_plugins::{PluginManager, PluginManifest};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

struct ActiveReply {
    session: String,
    message: String,
    content: String,
    model: String,
    events: mpsc::Receiver<AgentEvent>,
    trace: Vec<ToolRound>,
    tools: Arc<ToolSet>,
    result: oneshot::Receiver<Result<AgentOutcome, CoreError>>,
    cancel: CancellationToken,
    checkpoint: Instant,
}

#[derive(Clone)]
struct PendingReply {
    trace: Vec<ToolRound>,
    session: String,
    content: String,
    status: MessageStatus,
    notice: String,
    error: bool,
}

pub struct Ui {
    pub window: gtk::ApplicationWindow,
    store: RefCell<Store>,
    plugins: PluginManager,
    tools: Arc<ToolHub>,
    tool_button: gtk::Button,
    activity_button: gtk::Button,
    approval: RefCell<Option<Dialog>>,
    runtime: Arc<Runtime>,
    selected: RefCell<String>,
    sessions: gtk::ListBox,
    session_ids: RefCell<Vec<String>>,
    rebuilding_sessions: Cell<bool>,
    messages: Messages,
    history: gtk::Stack,
    composer: gtk::TextView,
    send: gtk::Button,
    stop: gtk::Button,
    send_stack: gtk::Stack,
    new_chat: gtk::Button,
    settings_button: gtk::Button,
    model_button: gtk::Button,
    plugin_button: gtk::Button,
    notice: gtk::Label,
    retry: gtk::Button,
    spinner: gtk::Spinner,
    key: RefCell<String>,
    active: RefCell<Option<ActiveReply>>,
    pending_reply: RefCell<Option<PendingReply>>,
    draft_changed: RefCell<Option<Instant>>,
    loading_draft: Cell<bool>,
    plugin_busy: Cell<bool>,
}

fn safe_text(text: &str) -> String {
    text.replace('\0', "�")
}

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}

fn button(text: &str, primary: bool) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    if primary {
        button.add_css_class("primary");
    }
    button
}

fn field(container: &gtk::Box, name: &str, widget: &impl IsA<gtk::Widget>) {
    let caption = label(name, "field-label");
    caption.set_mnemonic_widget(Some(widget));
    container.append(&caption);
    container.append(widget);
}

impl Ui {
    pub fn build(
        app: &gtk::Application,
        mut store: Store,
        profile: PathBuf,
    ) -> Result<Rc<Self>, String> {
        if store.sessions().is_empty() {
            store.create_session().map_err(|error| error.to_string())?;
        }
        let selected = store.sessions()[0].id.clone();
        let plugins = PluginManager::new(&profile).map_err(|error| error.to_string())?;
        let tools = ToolHub::open(&profile, plugins.clone())?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "The native background worker could not start.".to_owned())?,
        );
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("DeepSeek Harness for Linux — Native")
            .default_width(1120)
            .default_height(800)
            .width_request(640)
            .height_request(480)
            .build();
        window.add_css_class("native");
        let header = gtk::HeaderBar::new();
        let title = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        title.append(&label("DeepSeek Harness for Linux", "app-title"));
        title.append(&label("Native preview", "preview"));
        header.set_title_widget(Some(&title));
        let settings_button = gtk::Button::from_icon_name("emblem-system-symbolic");
        settings_button.set_tooltip_text(Some("Settings"));
        settings_button.update_property(&[gtk::accessible::Property::Label("Settings")]);
        let model_button = button(&safe_text(&store.settings().model), false);
        if let Some(caption) = model_button.child().and_downcast::<gtk::Label>() {
            caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
            caption.set_max_width_chars(18);
        }
        model_button.add_css_class("model-button");
        model_button.set_tooltip_text(Some("Change model in Settings"));
        header.pack_end(&settings_button);
        header.pack_end(&model_button);
        window.set_titlebar(Some(&header));

        let layout = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("sidebar");
        sidebar.set_width_request(220);
        let new_chat = button("+  New conversation", true);
        new_chat.update_property(&[gtk::accessible::Property::Label("New conversation")]);
        sidebar.append(&new_chat);
        sidebar.append(&label("CONVERSATIONS", "sidebar-heading"));
        let sessions = gtk::ListBox::new();
        sessions.add_css_class("session-list");
        sessions.set_selection_mode(gtk::SelectionMode::Single);
        sessions.update_property(&[gtk::accessible::Property::Label("Saved conversations")]);
        let session_scroll = gtk::ScrolledWindow::builder()
            .child(&sessions)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        sidebar.append(&session_scroll);
        let plugin_button = button("Extensions", false);
        plugin_button.set_tooltip_text(Some("Install and run native extensions"));
        sidebar.append(&plugin_button);
        let tool_button = button("Tool connections", false);
        sidebar.append(&tool_button);
        let footer = label("Unofficial community client", "sidebar-footer");
        footer.set_wrap(true);
        sidebar.append(&footer);
        layout.append(&sidebar);

        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.set_hexpand(true);
        let messages = Messages::new();
        let history = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty.set_valign(gtk::Align::Center);
        empty.set_halign(gtk::Align::Center);
        empty.set_margin_start(28);
        empty.set_margin_end(28);
        let mark = label("h↗", "empty-mark");
        mark.set_halign(gtk::Align::Center);
        let heading = label("What would you like to work on?", "empty-title");
        heading.set_wrap(true);
        heading.set_justify(gtk::Justification::Center);
        heading.set_xalign(0.5);
        let sub = label(
            "Saved locally. Sent to your model provider when you chat.",
            "empty-copy",
        );
        sub.set_wrap(true);
        sub.set_xalign(0.5);
        empty.append(&mark);
        empty.append(&heading);
        empty.append(&sub);
        history.add_named(&empty, Some("empty"));
        history.add_named(&messages.container, Some("messages"));
        main.append(&history);

        let composer_area = gtk::Box::new(gtk::Orientation::Vertical, 5);
        composer_area.add_css_class("composer-area");
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("composer-frame");
        let composer = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(0)
            .bottom_margin(0)
            .accepts_tab(false)
            .build();
        composer.add_css_class("composer");
        composer.update_property(&[gtk::accessible::Property::Label("Message")]);
        let input_scroll = gtk::ScrolledWindow::builder()
            .child(&composer)
            .min_content_height(86)
            .max_content_height(180)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        frame.append(&input_scroll);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.add_css_class("composer-actions");
        let helper = label("Ctrl+Enter to send · Enter for a new line", "helper");
        helper.set_hexpand(true);
        helper.set_wrap(true);
        actions.append(&helper);
        let spinner = gtk::Spinner::new();
        actions.append(&spinner);
        let send = button("Send", true);
        let stop = button("Stop", false);
        stop.set_tooltip_text(Some("Stop generating and keep the partial reply"));
        let send_stack = gtk::Stack::builder().width_request(88).build();
        send_stack.add_named(&send, Some("send"));
        send_stack.add_named(&stop, Some("stop"));
        actions.append(&send_stack);
        frame.append(&actions);
        composer_area.append(&frame);
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        status_row.set_height_request(37);
        let notice = label("Add an API key in Settings to start chatting.", "notice");
        notice.set_wrap(true);
        notice.set_hexpand(true);
        notice.update_property(&[gtk::accessible::Property::Label("Conversation status")]);
        let retry = button("Retry reply", false);
        retry.set_visible(false);
        status_row.append(&notice);
        let activity_button = button("Tool activity", false);
        activity_button.set_visible(false);
        status_row.append(&activity_button);
        status_row.append(&retry);
        composer_area.append(&status_row);
        main.append(&composer_area);
        layout.append(&main);
        window.set_child(Some(&layout));

        let ui = Rc::new(Self {
            window,
            store: RefCell::new(store),
            plugins,
            tools,
            tool_button,
            activity_button,
            approval: RefCell::new(None),
            runtime,
            selected: RefCell::new(selected),
            sessions,
            session_ids: RefCell::new(Vec::new()),
            rebuilding_sessions: Cell::new(false),
            messages,
            history,
            composer,
            send,
            stop,
            send_stack,
            new_chat,
            settings_button,
            model_button,
            plugin_button,
            notice,
            retry,
            spinner,
            key: RefCell::new(std::env::var("DEEPSEEK_API_KEY").unwrap_or_default()),
            active: RefCell::new(None),
            pending_reply: RefCell::new(None),
            draft_changed: RefCell::new(None),
            loading_draft: Cell::new(false),
            plugin_busy: Cell::new(false),
        });
        ui.connect();
        ui.refresh_sessions();
        ui.load_selected();
        if !ui.key.borrow().is_empty() {
            ui.status("Ready · conversations are saved locally.", false);
        }
        ui.controls();
        Ok(ui)
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.send.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.send_message(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.retry.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.pending_reply.borrow().is_some() {
                    ui.save_pending_reply();
                    ui.controls();
                } else {
                    ui.send_message(true);
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.stop.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if let Some(active) = ui.active.borrow().as_ref() {
                    active.cancel.cancel();
                }
                ui.status("Stopping…", false);
                ui.stop.set_sensitive(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.new_chat.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if !ui.flush_draft() {
                    return;
                }
                let created = ui.store.borrow_mut().create_session();
                match created {
                    Ok(session) => {
                        *ui.selected.borrow_mut() = session.id;
                        ui.refresh_sessions();
                        ui.load_selected();
                    }
                    Err(error) => ui.status(&error.to_string(), true),
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.sessions.connect_row_selected(move |_, row| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.rebuilding_sessions.get() || ui.active.borrow().is_some() {
                return;
            }
            let Some(row) = row else {
                return;
            };
            let Some(id) = ui.session_ids.borrow().get(row.index() as usize).cloned() else {
                return;
            };
            if id == *ui.selected.borrow() {
                return;
            }
            if !ui.flush_draft() {
                ui.refresh_sessions();
                return;
            }
            *ui.selected.borrow_mut() = id;
            ui.load_selected();
        });
        let weak = Rc::downgrade(self);
        self.messages.earlier.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.history_page(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.messages.latest.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.history_page(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.composer.buffer().connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                if !ui.loading_draft.get() {
                    *ui.draft_changed.borrow_mut() = Some(Instant::now());
                }
                ui.controls();
            }
        });
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        key.connect_key_pressed(move |_, value, _, modifiers| {
            if matches!(value, gdk::Key::Return | gdk::Key::KP_Enter)
                && modifiers.contains(gdk::ModifierType::CONTROL_MASK)
            {
                if let Some(ui) = weak.upgrade() {
                    ui.send_message(false);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.composer.add_controller(key);
        for control in [&self.settings_button, &self.model_button] {
            let weak = Rc::downgrade(self);
            control.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.settings();
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.plugin_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.extensions();
            }
        });
        let weak = Rc::downgrade(self);
        self.tool_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                crate::tools_ui::show(&ui.window, ui.tools.clone(), ui.runtime.clone());
            }
        });
        let weak = Rc::downgrade(self);
        self.activity_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.tool_activity();
            }
        });
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                if !ui.flush_draft() {
                    return glib::Propagation::Stop;
                }
                if !ui.shutdown() {
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(33), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            ui.poll();
            glib::ControlFlow::Continue
        });
    }

    fn input(&self) -> String {
        let buffer = self.composer.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn controls(&self) {
        let busy = self.active.borrow().is_some();
        let unsaved = self.pending_reply.borrow().is_some();
        self.send.set_sensitive(
            !busy && !unsaved && !self.plugin_busy.get() && !self.input().trim().is_empty(),
        );
        self.send_stack
            .set_visible_child_name(if busy { "stop" } else { "send" });
        self.new_chat.set_sensitive(!busy && !unsaved);
        self.sessions.set_sensitive(!busy && !unsaved);
        self.tool_button.set_sensitive(!busy && !unsaved);
        self.activity_button.set_sensitive(!busy);
        self.activity_button.set_visible(
            self.store
                .borrow()
                .session(&self.selected.borrow())
                .is_some_and(|session| {
                    session.messages.iter().any(|message| {
                        message
                            .tool_trace
                            .iter()
                            .any(|round| !round.tool_calls.is_empty())
                    })
                }),
        );
        self.settings_button.set_sensitive(!busy);
        self.model_button.set_sensitive(!busy);
        self.plugin_button
            .set_sensitive(!busy && !self.plugin_busy.get());
        if busy {
            self.spinner.start();
        } else {
            self.spinner.stop();
            self.stop.set_sensitive(true);
        }
        self.retry.set_sensitive(!busy && !self.plugin_busy.get());
    }

    fn status(&self, text: &str, error: bool) {
        self.notice.set_text(&safe_text(text));
        if error {
            self.notice.add_css_class("error");
        } else {
            self.notice.remove_css_class("error");
        }
    }

    fn refresh_sessions(&self) {
        self.rebuilding_sessions.set(true);
        while let Some(child) = self.sessions.first_child() {
            self.sessions.remove(&child);
        }
        let store = self.store.borrow();
        let mut ids = self.session_ids.borrow_mut();
        ids.clear();
        for session in store.sessions() {
            let title = label(&safe_text(&session.title), "session-title");
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.set_max_width_chars(22);
            title.set_tooltip_text(Some(&safe_text(&session.title)));
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&title));
            self.sessions.append(&row);
            ids.push(session.id.clone());
            if session.id == *self.selected.borrow() {
                self.sessions.select_row(Some(&row));
            }
        }
        self.rebuilding_sessions.set(false);
    }

    fn load_selected(&self) {
        let store = self.store.borrow();
        if let Some(session) = store.session(&self.selected.borrow()) {
            self.messages.replace(&session.messages);
            self.activity_button
                .set_visible(session.messages.iter().any(|message| {
                    message
                        .tool_trace
                        .iter()
                        .any(|round| !round.tool_calls.is_empty())
                }));
            self.history
                .set_visible_child_name(if session.messages.is_empty() {
                    "empty"
                } else {
                    "messages"
                });
            self.loading_draft.set(true);
            self.composer.buffer().set_text(&safe_text(&session.draft));
            self.loading_draft.set(false);
            self.retry
                .set_visible(session.messages.last().is_some_and(|message| {
                    message.role == Role::Assistant && message.status != MessageStatus::Complete
                }));
        }
        self.messages.bottom();
        self.composer.grab_focus();
    }

    fn history_page(&self, earlier: bool) {
        let id = self.selected.borrow().clone();
        let last_message = {
            let store = self.store.borrow();
            let Some(session) = store.session(&id) else {
                return;
            };
            if earlier {
                self.messages.show_earlier(&session.messages);
            } else {
                self.messages.show_latest(&session.messages);
            }
            session.messages.last().map(|message| message.id.clone())
        };
        if let Some(reply) = self
            .active
            .borrow()
            .as_ref()
            .filter(|reply| reply.session == id)
        {
            self.messages
                .update_last(&reply.message, &reply.content, MessageStatus::Streaming);
        }
        if let Some(pending) = self
            .pending_reply
            .borrow()
            .as_ref()
            .filter(|reply| reply.session == id)
        {
            if let Some(message) = last_message {
                self.messages
                    .update_last(&message, &pending.content, pending.status);
            }
        }
    }

    fn flush_draft(&self) -> bool {
        if self.draft_changed.borrow().is_none() {
            return true;
        }
        let result = self
            .store
            .borrow_mut()
            .save_draft(&self.selected.borrow(), self.input());
        match result {
            Ok(()) => {
                self.draft_changed.borrow_mut().take();
                true
            }
            Err(error) => {
                self.status(&error.to_string(), true);
                false
            }
        }
    }

    fn send_message(self: &Rc<Self>, retry: bool) {
        if self.active.borrow().is_some()
            || self.pending_reply.borrow().is_some()
            || self.plugin_busy.get()
        {
            return;
        }
        if !retry && self.input().trim().is_empty() {
            return;
        }
        if retry && !self.flush_draft() {
            return;
        }
        let tools = match self.tools.snapshot() {
            Ok(tools) => tools,
            Err(error) => {
                self.status(&error, true);
                return;
            }
        };
        let settings = self.store.borrow().settings().clone();
        if self.key.borrow().trim().is_empty()
            && !settings.base_url.starts_with("http://127.0.0.1")
            && !settings.base_url.starts_with("http://localhost")
        {
            self.status(
                "Add your API key in Settings, then send your message.",
                true,
            );
            self.settings();
            return;
        }
        let provider = match Provider::new() {
            Ok(value) => value,
            Err(error) => {
                self.status(&error.to_string(), true);
                return;
            }
        };
        let id = self.selected.borrow().clone();
        let snapshot;
        let message;
        {
            let mut store = self.store.borrow_mut();
            let result = if retry {
                store.append_message(&id, Role::Assistant, "", MessageStatus::Streaming)
            } else {
                store.begin_turn(&id, self.input().trim())
            };
            if let Err(error) = result {
                self.status(&error.to_string(), true);
                return;
            }
            let session = store.session(&id).expect("selected session");
            snapshot = if retry {
                let mut history = session.messages.clone();
                history.pop();
                while history.last().is_some_and(|message| {
                    message.role == Role::Assistant
                        && message.status != MessageStatus::Complete
                        && message.tool_trace.is_empty()
                }) {
                    history.pop();
                }
                history
            } else {
                session.messages.clone()
            };
            message = session.messages.last().expect("started reply").id.clone();
        }
        self.loading_draft.set(true);
        if !retry {
            self.composer.buffer().set_text("");
            self.draft_changed.borrow_mut().take();
        }
        self.loading_draft.set(false);
        let (sender, receiver) = mpsc::channel(64);
        let (result_sender, result) = oneshot::channel();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let key = self.key.borrow().clone();
        let model = settings.model.clone();
        let catalog = tools.clone();
        self.runtime.spawn(async move {
            let _cancel_on_exit = token.clone().drop_guard();
            let outcome = provider
                .run_agent(
                    &settings,
                    &key,
                    &snapshot,
                    &catalog.definitions,
                    sender,
                    token.clone(),
                )
                .await;
            token.cancel();
            let _ = result_sender.send(outcome);
        });
        *self.active.borrow_mut() = Some(ActiveReply {
            session: id,
            message,
            content: String::new(),
            model,
            events: receiver,
            trace: Vec::new(),
            tools,
            result,
            cancel,
            checkpoint: Instant::now(),
        });
        self.refresh_sessions();
        self.load_selected();
        self.retry.set_visible(false);
        self.status("Waiting for a reply…", false);
        self.controls();
    }

    fn poll(self: &Rc<Self>) {
        if self
            .draft_changed
            .borrow()
            .as_ref()
            .is_some_and(|changed| changed.elapsed() >= Duration::from_millis(750))
        {
            self.flush_draft();
        }
        let follow = self.messages.near_bottom();
        let mut finished = None;
        let mut request = None;
        {
            let mut active = self.active.borrow_mut();
            if let Some(reply) = active.as_mut() {
                let mut changed = false;
                while let Ok(event) = reply.events.try_recv() {
                    match event {
                        AgentEvent::Delta(delta) => {
                            reply.content.push_str(&delta);
                            changed = true;
                        }
                        AgentEvent::Checkpoint { trace, ack } => {
                            reply.trace = trace;
                            let saved = self.store.borrow_mut().replace_last_agent(
                                &reply.session,
                                &reply.content,
                                MessageStatus::Streaming,
                                reply.trace.clone(),
                            );
                            let success = saved.is_ok();
                            let _ = ack.send(success);
                            if let Err(error) = saved {
                                reply.cancel.cancel();
                                self.status(&error.to_string(), true);
                            }
                        }
                        AgentEvent::ToolRequest {
                            call,
                            reply: decision,
                        } => {
                            request =
                                Some((call, decision, reply.tools.clone(), reply.cancel.clone()));
                        }
                    }
                }
                if changed {
                    self.messages.update_last(
                        &reply.message,
                        &reply.content,
                        MessageStatus::Streaming,
                    );
                    self.status(
                        &format!("Generating with {}…", safe_text(&reply.model)),
                        false,
                    );
                    if follow {
                        self.messages.bottom();
                    }
                }
                if changed && reply.checkpoint.elapsed() >= Duration::from_secs(1) {
                    if let Err(error) = self.store.borrow_mut().replace_last_agent(
                        &reply.session,
                        &reply.content,
                        MessageStatus::Streaming,
                        reply.trace.clone(),
                    ) {
                        reply.cancel.cancel();
                        self.status(&error.to_string(), true);
                    }
                    reply.checkpoint = Instant::now();
                }
                match reply.result.try_recv() {
                    Ok(result) => finished = Some(result.map_err(|error| error.to_string())),
                    Err(oneshot::error::TryRecvError::Closed) => {
                        finished = Some(Err(
                            "The reply worker stopped. Review saved tool activity before retrying."
                                .to_owned(),
                        ))
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
        }
        if let Some((call, decision, tools, cancel)) = request {
            self.approve_tool(call, decision, tools, cancel);
        }
        if let Some(result) = finished {
            if let Some(dialog) = self.approval.borrow_mut().take() {
                dialog.close();
            }
            if let Some(mut reply) = self.active.borrow_mut().take() {
                drain_reply(&mut reply);
                let (status, text, error) = match result {
                    Ok(outcome) => {
                        reply.trace = outcome.trace;
                        match outcome.outcome {
                            StreamOutcome::Completed => (
                                MessageStatus::Complete,
                                "Reply saved locally.".to_owned(),
                                false,
                            ),
                            StreamOutcome::Cancelled => (
                                MessageStatus::Cancelled,
                                "Stopped. Review saved tool activity before retrying.".to_owned(),
                                false,
                            ),
                        }
                    }
                    Err(error) => (MessageStatus::Error, error, true),
                };
                self.messages
                    .update_last(&reply.message, &reply.content, status);
                *self.pending_reply.borrow_mut() = Some(PendingReply {
                    trace: reply.trace,
                    session: reply.session,
                    content: reply.content,
                    status,
                    notice: text,
                    error,
                });
                self.save_pending_reply();
            }
            self.controls();
            if follow {
                self.messages.bottom();
            }
        }
    }

    fn approve_tool(
        self: &Rc<Self>,
        call: ToolCall,
        decision: oneshot::Sender<ToolDecision>,
        tools: Arc<ToolSet>,
        cancel: CancellationToken,
    ) {
        if cancel.is_cancelled() {
            let _ = decision.send(ToolDecision::Denied);
            return;
        }
        let dialog = Dialog::new("Allow tool call?", &self.window, 600);
        dialog.add_button("Deny", gtk::ResponseType::Cancel);
        dialog.add_button("Stop task", gtk::ResponseType::Reject);
        dialog.add_button("Allow once", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let area = dialog.content_area();
        area.set_spacing(12);
        area.set_margin_start(24);
        area.set_margin_end(24);
        area.set_margin_top(20);
        area.set_margin_bottom(16);
        area.append(&label(
            "The model requests an external action. Allow only if these details match your task.",
            "notice",
        ));
        let name = label(&safe_text(&call.name), "message-author");
        name.set_wrap(true);
        area.append(&name);
        let arguments = label(
            &safe_text(&serde_json::to_string_pretty(&call.arguments).unwrap_or_default()),
            "notice",
        );
        arguments.set_wrap(true);
        arguments.set_selectable(true);
        arguments.set_width_chars(40);
        arguments.update_property(&[gtk::accessible::Property::Label("Tool arguments")]);
        area.append(&arguments);
        self.status(
            &format!("Waiting for permission: {}", safe_text(&call.name)),
            false,
        );
        let decision = Rc::new(RefCell::new(Some(decision)));
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |dialog, response| {
            let Some(sender) = decision.borrow_mut().take() else {
                return;
            };
            let Some(ui) = weak.upgrade() else {
                let _ = sender.send(ToolDecision::Denied);
                dialog.close();
                return;
            };
            if response == gtk::ResponseType::Accept
                && !cancel.is_cancelled()
                && !sender.is_closed()
            {
                ui.status(&format!("Running {}…", safe_text(&call.name)), false);
                let tools = tools.clone();
                let call = call.clone();
                let cancel = cancel.clone();
                ui.runtime.spawn(async move {
                    let result = tools.execute(&call, cancel).await;
                    let _ = sender.send(match result {
                        Ok(text) => ToolDecision::Result(text),
                        Err(error) => ToolDecision::Failed(error),
                    });
                });
            } else {
                if response == gtk::ResponseType::Reject {
                    cancel.cancel();
                }
                let _ = sender.send(ToolDecision::Denied);
            }
            dialog.close();
        });
        *self.approval.borrow_mut() = Some(dialog.clone());
        dialog.present();
    }

    fn tool_activity(&self) {
        let dialog = Dialog::new("Tool activity", &self.window, 620);
        dialog.add_button("Close", gtk::ResponseType::Close);
        let area = dialog.content_area();
        area.set_spacing(14);
        area.set_margin_start(24);
        area.set_margin_end(24);
        area.set_margin_top(20);
        area.set_margin_bottom(16);
        area.append(&label("Latest 50 saved tool calls. Arguments and results are previews; no action runs from this view.", "notice"));
        let store = self.store.borrow();
        if let Some(session) = store.session(&self.selected.borrow()) {
            let calls = session
                .messages
                .iter()
                .rev()
                .flat_map(|message| message.tool_trace.iter().rev())
                .flat_map(|round| round.tool_calls.iter().rev().map(move |call| (round, call)))
                .take(50);
            for (round, call) in calls {
                let section = gtk::Box::new(gtk::Orientation::Vertical, 7);
                section.add_css_class("plugin-row");
                section.append(&label(&safe_text(&call.name), "message-author"));
                let result = round
                    .results
                    .iter()
                    .find(|result| result.tool_call_id == call.id);
                section.append(&label(
                    match result {
                        Some(result) if result.is_error => "Declined, failed or interrupted",
                        Some(_) => "Completed",
                        None => "No confirmed result · check the external action before retrying",
                    },
                    "notice",
                ));
                let details = gtk::Expander::builder()
                    .label("Arguments and result")
                    .build();
                let text = format!(
                    "Arguments\n{}\n\nResult\n{}",
                    serde_json::to_string_pretty(&call.arguments).unwrap_or_default(),
                    result.map_or("No confirmed result", |result| result.content.as_str())
                );
                let preview: String = text.chars().take(8192).collect();
                let body = label(&safe_text(&preview), "notice");
                body.set_wrap(true);
                body.set_selectable(true);
                body.set_width_chars(40);
                details.set_child(Some(&body));
                section.append(&details);
                area.append(&section);
            }
        }
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }

    fn save_pending_reply(&self) -> bool {
        let Some(pending) = self.pending_reply.borrow().clone() else {
            return true;
        };
        let saved = self.store.borrow_mut().replace_last_agent(
            &pending.session,
            &pending.content,
            pending.status,
            pending.trace.clone(),
        );
        match saved {
            Ok(()) => {
                self.pending_reply.borrow_mut().take();
                self.retry.set_label("Retry reply");
                self.retry
                    .set_visible(pending.status != MessageStatus::Complete);
                self.status(&pending.notice, pending.error);
                true
            }
            Err(error) => {
                self.status(
                    &format!(
                        "Reply not saved. {error} Use Retry save after fixing the storage problem."
                    ),
                    true,
                );
                self.retry.set_label("Retry save");
                self.retry.set_visible(true);
                false
            }
        }
    }

    pub fn shutdown(&self) -> bool {
        if !self.flush_draft() {
            return false;
        }
        if let Some(dialog) = self.approval.borrow_mut().take() {
            dialog.close();
        }
        let follow = self.messages.near_bottom();
        if let Some(mut reply) = self.active.borrow_mut().take() {
            reply.cancel.cancel();
            drain_reply(&mut reply);
            self.messages
                .update_last(&reply.message, &reply.content, MessageStatus::Cancelled);
            *self.pending_reply.borrow_mut() = Some(PendingReply {
                trace: reply.trace,
                session: reply.session,
                content: reply.content,
                status: MessageStatus::Cancelled,
                notice: "Stopped. The partial reply is saved.".to_owned(),
                error: false,
            });
        }
        let saved = self.save_pending_reply();
        self.controls();
        if follow {
            self.messages.bottom();
        }
        if !saved {
            return false;
        }
        self.runtime.block_on(self.tools.close_all());
        self.key.borrow_mut().clear();
        true
    }

    fn settings(self: &Rc<Self>) {
        if self.active.borrow().is_some() {
            return;
        }
        let dialog = Dialog::new("Settings", &self.window, 520);
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Save settings", gtk::ResponseType::Accept);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.add_css_class("dialog-content");
        content.append(&label("Model connection", "section-title"));
        let settings = self.store.borrow().settings().clone();
        let model = gtk::Entry::builder()
            .text(safe_text(&settings.model))
            .build();
        model.update_property(&[gtk::accessible::Property::Label("Model")]);
        field(&content, "Model", &model);
        let endpoint = gtk::Entry::builder()
            .text(safe_text(&settings.base_url))
            .build();
        endpoint.update_property(&[gtk::accessible::Property::Label("API endpoint")]);
        field(&content, "API endpoint", &endpoint);
        let key = gtk::PasswordEntry::builder()
            .show_peek_icon(true)
            .text(&*self.key.borrow())
            .build();
        key.update_property(&[gtk::accessible::Property::Label("API key")]);
        field(&content, "API key", &key);
        let hint = label("Your API key stays in memory for this app session. Conversations and connection settings are saved locally.", "helper");
        hint.set_wrap(true);
        content.append(&hint);
        let instructions = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .build();
        instructions
            .buffer()
            .set_text(&safe_text(&settings.system_prompt));
        instructions.update_property(&[gtk::accessible::Property::Label("Custom instructions")]);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&instructions)
            .min_content_height(90)
            .max_content_height(140)
            .build();
        field(&content, "Custom instructions", &scroller);
        let error = label("", "notice");
        error.add_css_class("error");
        error.set_wrap(true);
        content.append(&error);
        dialog.content_area().append(&content);
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(ui) = weak.upgrade() {
                    let buffer = instructions.buffer();
                    let settings = Settings {
                        model: model.text().trim().to_owned(),
                        base_url: endpoint.text().trim().to_owned(),
                        system_prompt: buffer
                            .text(&buffer.start_iter(), &buffer.end_iter(), false)
                            .to_string(),
                    };
                    let saved = ui.store.borrow_mut().save_settings(settings);
                    if let Err(failure) = saved {
                        error.set_text(&failure.to_string());
                        model.grab_focus();
                        return;
                    }
                    *ui.key.borrow_mut() = key.text().to_string();
                    ui.model_button
                        .set_label(&safe_text(&ui.store.borrow().settings().model));
                    ui.status(
                        "Settings saved. Your API key is kept for this app session only.",
                        false,
                    );
                }
            }
            key.set_text("");
            dialog.close();
        });
        dialog.present();
    }

    fn extensions(self: &Rc<Self>) {
        let dialog = Dialog::new("Extensions", &self.window, 560);
        dialog.add_button("Close", gtk::ResponseType::Close);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.add_css_class("dialog-content");
        content.append(&label("Native extensions", "section-title"));
        let intro = label("Run an extension on the text in your message box. Install a native extension folder containing its manifest and WASM file.", "helper");
        intro.set_wrap(true);
        content.append(&intro);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .min_content_height(150)
            .max_content_height(380)
            .build();
        content.append(&scroll);
        let feedback = label("", "notice");
        feedback.set_wrap(true);
        content.append(&feedback);
        let install = button("Install extension…", true);
        content.append(&install);
        let caveat = label("Harness JavaScript plugins use a different format. This preview supports native text extensions.", "helper");
        caveat.set_wrap(true);
        content.append(&caveat);
        dialog.content_area().append(&content);
        self.extension_rows(&list, &feedback, dialog.window());
        let weak = Rc::downgrade(self);
        let list_copy = list.clone();
        let feedback_copy = feedback.clone();
        let dialog_copy = dialog.clone();
        install.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let weak = Rc::downgrade(&ui);
            let list = list_copy.clone();
            let feedback = feedback_copy.clone();
            let parent = dialog_copy.window().clone();
            dialogs::choose_folder(dialog_copy.window(), move |choice| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let path = match choice {
                    Ok(Some(path)) => path,
                    Ok(None) => return,
                    Err(error) => {
                        feedback.set_text(&error);
                        return;
                    }
                };
                let manager = ui.plugins.clone();
                let (sender, mut receiver) = oneshot::channel();
                ui.runtime.spawn_blocking(move || {
                    let _ = sender.send(manager.install(path));
                });
                feedback.set_text("Installing extension…");
                let weak = Rc::downgrade(&ui);
                let list = list.clone();
                let feedback = feedback.clone();
                glib::timeout_add_local(Duration::from_millis(40), move || {
                    match receiver.try_recv() {
                        Ok(result) => {
                            if let Some(ui) = weak.upgrade() {
                                match result {
                                    Ok(_) => {
                                        ui.extension_rows(&list, &feedback, &parent);
                                        feedback.set_text("Extension installed.");
                                    }
                                    Err(error) => feedback.set_text(&error.to_string()),
                                }
                            }
                            glib::ControlFlow::Break
                        }
                        Err(oneshot::error::TryRecvError::Empty) => glib::ControlFlow::Continue,
                        Err(_) => {
                            feedback
                                .set_text("The extension worker stopped. Try installing again.");
                            glib::ControlFlow::Break
                        }
                    }
                });
            });
        });
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.present();
    }

    fn extension_rows(
        self: &Rc<Self>,
        list: &gtk::Box,
        feedback: &gtk::Label,
        parent: &gtk::Window,
    ) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        match self.plugins.list() {
            Ok(plugins) if plugins.is_empty() => {
                list.append(&label("No extensions installed yet.", "empty-copy"));
            }
            Ok(plugins) => {
                for plugin in plugins {
                    self.extension_row(list, feedback, parent, plugin);
                }
            }
            Err(error) => feedback.set_text(&error.to_string()),
        }
    }

    fn extension_row(
        self: &Rc<Self>,
        list: &gtk::Box,
        feedback: &gtk::Label,
        parent: &gtk::Window,
        plugin: PluginManifest,
    ) {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 7);
        row.add_css_class("plugin-row");
        let name = label(&safe_text(&plugin.name), "message-author");
        name.set_wrap(true);
        row.append(&name);
        let description = label(&safe_text(&plugin.description), "helper");
        description.set_wrap(true);
        row.append(&description);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let run = button(&safe_text(&plugin.command), false);
        run.set_tooltip_text(Some("Apply to the current message draft"));
        let remove = button("Remove", false);
        actions.append(&run);
        actions.append(&remove);
        row.append(&actions);
        list.append(&row);
        let weak = Rc::downgrade(self);
        let id = plugin.id.clone();
        let feedback_copy = feedback.clone();
        run.connect_clicked(move |control| {
            let Some(ui) = weak.upgrade() else { return; };
            let input = ui.input();
            if input.is_empty() { feedback_copy.set_text("Write text in your message box before running this extension."); return; }
            if ui.plugin_busy.replace(true) { return; }
            control.set_sensitive(false); ui.controls();
            let manager = ui.plugins.clone(); let id = id.clone(); let original = input.clone(); let session_id = ui.selected.borrow().clone();
            let (sender, mut receiver) = oneshot::channel();
            ui.runtime.spawn_blocking(move || { let _ = sender.send(manager.run(&id, &input)); });
            let weak = Rc::downgrade(&ui); let feedback = feedback_copy.clone(); let control = control.clone();
            glib::timeout_add_local(Duration::from_millis(33), move || {
                let result = match receiver.try_recv() { Ok(result) => result, Err(oneshot::error::TryRecvError::Empty) => return glib::ControlFlow::Continue, Err(_) => { feedback.set_text("The extension worker stopped. Your draft is unchanged."); if let Some(ui) = weak.upgrade() { ui.plugin_busy.set(false); ui.controls(); } return glib::ControlFlow::Break; } };
                if let Some(ui) = weak.upgrade() {
                    ui.plugin_busy.set(false); control.set_sensitive(true);
                    match result {
                        Ok(output) if *ui.selected.borrow() == session_id && ui.input() == original => { ui.composer.buffer().set_text(&safe_text(&output)); feedback.set_text("Extension applied to your draft."); },
                        Ok(_) => feedback.set_text("Your draft changed while the extension ran. Run it again to use the latest text."),
                        Err(error) => feedback.set_text(&error.to_string()),
                    }
                    ui.controls();
                }
                glib::ControlFlow::Break
            });
        });
        let weak = Rc::downgrade(self);
        let list = list.clone();
        let feedback = feedback.clone();
        let parent = parent.clone();
        remove.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let confirm = Dialog::new("Remove extension?", &parent, 420);
            confirm.add_button("Keep extension", gtk::ResponseType::Cancel);
            let remove_button = confirm.add_button("Remove extension", gtk::ResponseType::Accept);
            remove_button.remove_css_class("suggested-action");
            remove_button.add_css_class("destructive-action");
            confirm.set_default_response(gtk::ResponseType::Cancel);
            let copy = label(
                &format!(
                    "Remove {} from this client? Your conversations will stay saved.",
                    safe_text(&plugin.name)
                ),
                "notice",
            );
            copy.set_wrap(true);
            copy.set_margin_top(18);
            copy.set_margin_bottom(18);
            copy.set_margin_start(18);
            copy.set_margin_end(18);
            confirm.content_area().append(&copy);
            let weak = Rc::downgrade(&ui);
            let id = plugin.id.clone();
            let list = list.clone();
            let feedback = feedback.clone();
            let parent = parent.clone();
            confirm.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept {
                    if let Some(ui) = weak.upgrade() {
                        match ui.plugins.remove(&id) {
                            Ok(()) => {
                                ui.extension_rows(&list, &feedback, &parent);
                                feedback.set_text("Extension removed.");
                            }
                            Err(error) => feedback.set_text(&error.to_string()),
                        }
                    }
                }
                dialog.close();
            });
            confirm.present();
        });
    }
}

pub fn startup_error(app: &gtk::Application, message: &str) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("DeepSeek Harness for Linux — Native")
        .default_width(540)
        .default_height(260)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(28);
    content.set_margin_bottom(28);
    content.set_margin_start(28);
    content.set_margin_end(28);
    content.append(&label(
        "The native workspace could not open",
        "section-title",
    ));
    let detail = label(message, "notice");
    detail.set_wrap(true);
    content.append(&detail);
    let close = button("Close", false);
    let copy = window.clone();
    close.connect_clicked(move |_| copy.close());
    content.append(&close);
    window.set_child(Some(&content));
    window.present();
}

fn drain_reply(reply: &mut ActiveReply) {
    while let Ok(event) = reply.events.try_recv() {
        match event {
            AgentEvent::Delta(delta) => reply.content.push_str(&delta),
            AgentEvent::Checkpoint { trace, ack } => {
                reply.trace = trace;
                let _ = ack.send(false);
            }
            AgentEvent::ToolRequest { reply, .. } => {
                let _ = reply.send(ToolDecision::Denied);
            }
        }
    }
}
