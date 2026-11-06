use gtk::{gdk, glib, prelude::*};
use harness_native_core::{Message, MessageStatus, Role};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Page {
    start: usize,
    end: usize,
    total: usize,
}

impl Page {
    fn latest(total: usize) -> Self {
        Self {
            start: total.saturating_sub(PAGE_SIZE),
            end: total,
            total,
        }
    }

    fn earlier(self, total: usize) -> Option<Self> {
        let end = self.start.min(total);
        (end > 0).then(|| Self {
            start: end.saturating_sub(PAGE_SIZE),
            end,
            total,
        })
    }

    fn is_latest(self) -> bool {
        self.end == self.total
    }
}

struct MessageRow {
    id: String,
    content: String,
    status: MessageStatus,
    body: gtk::Label,
    state: gtk::Label,
}

#[derive(Default)]
struct ScrollState {
    latest_page: Cell<bool>,
    follow: Cell<bool>,
    pending_bottom: Cell<bool>,
    paused_by_user: Cell<bool>,
    programmatic: Cell<bool>,
    generation: Cell<u64>,
}

impl ScrollState {
    fn user_scrolled(&self, bottom_distance: f64) {
        if self.programmatic.get() {
            return;
        }
        self.pending_bottom.set(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        if bottom_distance <= 1.0 {
            self.paused_by_user.set(false);
        }
        self.follow
            .set(self.latest_page.get() && !self.paused_by_user.get() && bottom_distance < 100.0);
    }
}

pub struct Messages {
    pub container: gtk::Box,
    pub scroll: gtk::ScrolledWindow,
    pub earlier: gtk::Button,
    pub latest: gtk::Button,
    body: gtk::Box,
    pager: gtk::Box,
    range: gtk::Label,
    rows: RefCell<Vec<MessageRow>>,
    page: Cell<Page>,
    scrolling: Rc<ScrollState>,
}

fn status_label(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Complete => "",
        MessageStatus::Streaming => "Generating…",
        MessageStatus::Cancelled => "Stopped · partial reply saved",
        MessageStatus::Interrupted => "Interrupted · partial reply saved",
        MessageStatus::Error => "Reply failed",
    }
}

fn bottom_distance(adjustment: &gtk::Adjustment) -> f64 {
    adjustment.upper() - adjustment.page_size() - adjustment.value()
}

fn align_bottom(scroll: &gtk::ScrolledWindow, state: &ScrollState) {
    let adjustment = scroll.vadjustment();
    state.programmatic.set(true);
    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
    state.programmatic.set(false);
    if let Some(viewport) = scroll.child() {
        viewport.queue_allocate();
    }
}

impl Messages {
    pub fn new() -> Self {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        container.set_hexpand(true);
        container.set_vexpand(true);
        let pager = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        pager.add_css_class("history-navigation");
        pager.set_margin_top(8);
        pager.set_margin_bottom(8);
        pager.set_margin_start(24);
        pager.set_margin_end(24);
        let earlier = gtk::Button::with_label("Earlier messages");
        earlier.set_tooltip_text(Some("Show the previous 50 messages"));
        let latest = gtk::Button::with_label("Latest messages");
        latest.set_tooltip_text(Some("Return to the most recent messages"));
        let range = gtk::Label::new(Some("No messages"));
        range.add_css_class("helper");
        range.set_hexpand(true);
        range.set_wrap(true);
        range.set_xalign(0.5);
        pager.append(&earlier);
        pager.append(&range);
        pager.append(&latest);
        pager.set_visible(false);
        container.append(&pager);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("conversation");
        body.set_hexpand(true);
        body.set_valign(gtk::Align::Start);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&body)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        scroll.update_property(&[gtk::accessible::Property::Label("Conversation history")]);
        container.append(&scroll);
        let scrolling = Rc::new(ScrollState::default());
        let state = scrolling.clone();
        let weak_scroll = scroll.downgrade();
        scroll.vadjustment().connect_changed(move |_| {
            if state.pending_bottom.get() || state.follow.get() {
                let weak_scroll = weak_scroll.clone();
                let state = state.clone();
                // Adjustment::changed can run inside GtkViewport allocation.
                // Changing its value there leaves the child's paint transform
                // stale even though the scrollbar reports the requested value.
                glib::idle_add_local_once(move || {
                    if let Some(scroll) = weak_scroll.upgrade() {
                        if state.pending_bottom.get() || state.follow.get() {
                            align_bottom(&scroll, &state);
                        }
                    }
                });
            }
        });
        let state = scrolling.clone();
        scroll
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                if !state.pending_bottom.get() {
                    state.user_scrolled(bottom_distance(adjustment));
                }
            });
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        let state = scrolling.clone();
        let weak_scroll = scroll.downgrade();
        wheel.connect_scroll(move |_, _, direction| {
            if direction < 0.0 {
                state.paused_by_user.set(true);
                state.user_scrolled(f64::INFINITY);
            } else if direction > 0.0 {
                state.paused_by_user.set(false);
                if let Some(scroll) = weak_scroll.upgrade() {
                    state.user_scrolled(bottom_distance(&scroll.vadjustment()));
                }
            }
            glib::Propagation::Proceed
        });
        scroll.add_controller(wheel);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let state = scrolling.clone();
        let weak_scroll = scroll.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gdk::Key::Up | gdk::Key::Page_Up | gdk::Key::Home) {
                state.paused_by_user.set(true);
                state.user_scrolled(f64::INFINITY);
            } else if matches!(key, gdk::Key::Down | gdk::Key::Page_Down | gdk::Key::End) {
                state.paused_by_user.set(false);
                if let Some(scroll) = weak_scroll.upgrade() {
                    state.user_scrolled(bottom_distance(&scroll.vadjustment()));
                }
            }
            glib::Propagation::Proceed
        });
        scroll.add_controller(keys);
        Self {
            container,
            scroll,
            earlier,
            latest,
            body,
            pager,
            range,
            rows: RefCell::new(Vec::new()),
            page: Cell::new(Page::default()),
            scrolling,
        }
    }

    /// A new selection opens its latest page. Only that page owns native rows.
    pub fn replace(&self, messages: &[Message]) {
        self.render_page(messages, Page::latest(messages.len()));
        self.bottom();
    }

    pub fn show_earlier(&self, messages: &[Message]) {
        let Some(page) = self.page.get().earlier(messages.len()) else {
            return;
        };
        self.render_page(messages, page);
        // The end of this page is adjacent to the page the person just left.
        self.bottom();
    }

    pub fn show_latest(&self, messages: &[Message]) {
        self.replace(messages);
    }

    fn render_page(&self, messages: &[Message], page: Page) {
        self.scrolling.follow.set(false);
        self.scrolling.pending_bottom.set(false);
        self.scrolling
            .generation
            .set(self.scrolling.generation.get().wrapping_add(1));
        self.page.set(page);
        self.scrolling.latest_page.set(page.is_latest());
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        self.rows.borrow_mut().clear();
        for message in &messages[page.start..page.end] {
            self.add_row(message);
        }
        self.earlier.set_sensitive(page.start > 0);
        self.latest.set_sensitive(!page.is_latest());
        self.pager.set_visible(page.total > PAGE_SIZE);
        if messages.is_empty() {
            self.range.set_text("No messages");
        } else {
            self.range.set_text(&format!(
                "Messages {}–{} of {}",
                page.start + 1,
                page.end,
                page.total
            ));
        }
    }

    fn add_row(&self, message: &Message) {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
        row.add_css_class("message-row");
        row.set_hexpand(true);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let author = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        author.add_css_class("message-author");
        author.set_text(match message.role {
            Role::User => "You",
            Role::Assistant => "DeepSeek",
            Role::System => "Instructions",
        });
        let state = gtk::Label::new(Some(status_label(message.status)));
        state.add_css_class("message-state");
        state.set_wrap(true);
        state.set_xalign(1.0);
        header.append(&author);
        header.append(&state);
        let body = gtk::Label::builder()
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .yalign(0.0)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .width_chars(1)
            .build();
        body.add_css_class("message-body");
        body.update_property(&[gtk::accessible::Property::Label("Conversation message")]);
        let text = message.content.replace('\0', "�");
        if message.status == MessageStatus::Streaming {
            body.set_text(&text);
        } else {
            crate::markdown::render_label(&body, &text);
        }
        if message.role == Role::User {
            row.add_css_class("user-message");
        }
        row.append(&header);
        row.append(&body);
        self.body.append(&row);
        self.rows.borrow_mut().push(MessageRow {
            id: message.id.clone(),
            content: message.content.clone(),
            status: message.status,
            body,
            state,
        });
    }

    /// A response on another history page must never change the visible page.
    pub fn update_last(&self, id: &str, content: &str, status: MessageStatus) {
        let mut rows = self.rows.borrow_mut();
        let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        if row.content != content || row.status != status {
            let text = content.replace('\0', "�");
            if status == MessageStatus::Streaming {
                row.body.set_attributes(None);
                row.body.set_text(&text);
            } else {
                crate::markdown::render_label(&row.body, &text);
            }
            row.content = content.to_owned();
            row.status = status;
            row.state.set_text(status_label(status));
        }
    }

    pub fn near_bottom(&self) -> bool {
        self.scrolling.latest_page.get() && self.scrolling.follow.get()
    }

    /// Follow the newest reply through layout changes until the user scrolls up.
    pub fn bottom(&self) {
        self.scrolling.paused_by_user.set(false);
        self.scrolling.follow.set(self.scrolling.latest_page.get());
        self.scrolling.pending_bottom.set(true);
        let generation = self.scrolling.generation.get().wrapping_add(1);
        self.scrolling.generation.set(generation);
        align_bottom(&self.scroll, &self.scrolling);
        let state = self.scrolling.clone();
        let frames = Cell::new(0);
        self.scroll.add_tick_callback(move |scroll, _| {
            if generation != state.generation.get() || !state.pending_bottom.get() {
                return glib::ControlFlow::Break;
            }
            align_bottom(scroll, &state);
            frames.set(frames.get() + 1);
            if frames.get() >= 2 {
                state.pending_bottom.set(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_visits_every_message_once_without_rendering_more_than_fifty() {
        for total in 0..=501 {
            let mut page = Page::latest(total);
            let mut visited = Vec::new();
            loop {
                assert!(page.start <= page.end && page.end <= total);
                assert!(page.end - page.start <= PAGE_SIZE);
                assert_eq!(page.total, total);
                visited.extend((page.start..page.end).rev());
                match page.earlier(total) {
                    Some(earlier) => {
                        assert_eq!(
                            earlier.end, page.start,
                            "Adjacent pages do not repeat or skip messages"
                        );
                        page = earlier;
                    }
                    None => break,
                }
            }
            assert_eq!(visited, (0..total).rev().collect::<Vec<_>>());
        }
    }

    #[test]
    fn new_messages_refresh_the_total_without_moving_an_earlier_page_anchor() {
        let latest = Page::latest(120);
        let earlier = latest.earlier(122).unwrap();
        assert_eq!(
            earlier,
            Page {
                start: 20,
                end: 70,
                total: 122
            }
        );
        assert!(!earlier.is_latest());
        let first = earlier.earlier(122).unwrap();
        assert_eq!(
            first,
            Page {
                start: 0,
                end: 20,
                total: 122
            }
        );
        assert!(first.earlier(122).is_none());
        assert_eq!(
            Page::latest(122),
            Page {
                start: 72,
                end: 122,
                total: 122
            }
        );
    }

    #[test]
    fn user_scrolling_up_cancels_a_queued_bottom_alignment() {
        let state = ScrollState::default();
        state.latest_page.set(true);
        state.follow.set(true);
        state.pending_bottom.set(true);
        state.paused_by_user.set(true);
        let queued_frame = state.generation.get();
        state.user_scrolled(350.0);
        assert!(!state.follow.get());
        assert!(!state.pending_bottom.get());
        assert_ne!(state.generation.get(), queued_frame);
        state.user_scrolled(20.0);
        assert!(
            !state.follow.get(),
            "A small scroll-up keeps reading anchored even near the bottom"
        );
        state.user_scrolled(0.0);
        assert!(
            state.follow.get(),
            "Returning to the latest page's bottom resumes following"
        );
        state.latest_page.set(false);
        state.user_scrolled(0.0);
        assert!(
            !state.follow.get(),
            "Reading an earlier page never follows the newest response"
        );
    }

    #[test]
    fn programmatic_alignment_does_not_cancel_its_own_pending_layout_work() {
        let state = ScrollState::default();
        state.latest_page.set(true);
        state.follow.set(true);
        state.pending_bottom.set(true);
        state.programmatic.set(true);
        let generation = state.generation.get();
        state.user_scrolled(200.0);
        assert!(state.follow.get());
        assert!(state.pending_bottom.get());
        assert_eq!(state.generation.get(), generation);
    }
}
