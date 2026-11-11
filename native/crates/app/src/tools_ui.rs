//! Native setup for opt-in MCP processes and the optional JavaScript tool host.

use crate::dialogs::Dialog;
use crate::tools::ToolHub;
use gtk::{glib, prelude::*};
use harness_native_mcp::McpServerConfig;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{runtime::Runtime, sync::oneshot};
use tokio_util::sync::CancellationToken;

fn caption(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}

fn entry(area: &gtk::Box, title: &str, value: &str) -> gtk::Entry {
    let label = caption(title);
    let entry = gtk::Entry::new();
    entry.set_text(value);
    entry.update_property(&[gtk::accessible::Property::Label(title)]);
    label.set_mnemonic_widget(Some(&entry));
    area.append(&label);
    area.append(&entry);
    entry
}

fn content(dialog: &Dialog) -> gtk::Box {
    let area = dialog.content_area();
    area.set_spacing(10);
    area.set_margin_start(24);
    area.set_margin_end(24);
    area.set_margin_top(20);
    area.set_margin_bottom(16);
    area
}

pub fn show(parent: &gtk::ApplicationWindow, hub: Arc<ToolHub>, runtime: Arc<Runtime>) {
    let dialog = Dialog::new("Tool connections", parent, 600);
    dialog.add_button("Close", gtk::ResponseType::Close);
    let area = content(&dialog);
    let intro = caption("Connect external tools for this app session. Connections stay stopped after reopening the app. The model asks before each tool call.");
    area.append(&intro);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let add = gtk::Button::with_label("Add MCP server");
    let javascript = gtk::Button::with_label("Add JavaScript tool plugin");
    actions.append(&add);
    actions.append(&javascript);
    area.append(&actions);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    area.append(&list);
    let feedback = caption("");
    feedback.add_css_class("notice");
    area.append(&feedback);
    let wasm = gtk::CheckButton::with_label("Offer WASM extensions to the model");
    wasm.set_active(hub.wasm_enabled());
    area.append(&wasm);
    let hub_copy = hub.clone();
    let feedback_copy = feedback.clone();
    wasm.connect_toggled(move |control| {
        if let Err(error) = hub_copy.enable_wasm(control.is_active()) {
            feedback_copy.set_text(&error);
            control.set_active(hub_copy.wasm_enabled());
        }
    });
    refresh(&list, &feedback, dialog.window(), &hub, &runtime);
    let parent = dialog.window().clone();
    let hub_copy = hub.clone();
    let runtime_copy = runtime.clone();
    let list_copy = list.clone();
    let feedback_copy = feedback.clone();
    add.connect_clicked(move |_| {
        add_connection(
            &parent,
            &hub_copy,
            &runtime_copy,
            &list_copy,
            &feedback_copy,
            false,
        );
    });
    let parent = dialog.window().clone();
    javascript.connect_clicked(move |_| {
        add_connection(&parent, &hub, &runtime, &list, &feedback, true);
    });
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn refresh(
    list: &gtk::Box,
    feedback: &gtk::Label,
    parent: &gtk::Window,
    hub: &Arc<ToolHub>,
    runtime: &Arc<Runtime>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let connections = hub.connections();
    if connections.is_empty() {
        list.append(&caption(
            "No external tools connected yet. Installed WASM extensions are available separately.",
        ));
    }
    for (config, count) in connections {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 7);
        row.add_css_class("plugin-row");
        let heading = caption(&config.id);
        heading.add_css_class("message-author");
        row.append(&heading);
        row.append(&caption(&count.map_or_else(
            || "Stopped".to_owned(),
            |count| format!("Connected · {count} tools"),
        )));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let toggle = gtk::Button::with_label(if count.is_some() {
            "Disconnect"
        } else {
            "Connect"
        });
        let remove = gtk::Button::with_label("Remove connection");
        remove.set_sensitive(count.is_none());
        actions.append(&toggle);
        actions.append(&remove);
        row.append(&actions);
        list.append(&row);
        let hub_copy = hub.clone();
        let runtime_copy = runtime.clone();
        let list_copy = list.clone();
        let feedback_copy = feedback.clone();
        let parent_copy = parent.clone();
        let id = config.id.clone();
        toggle.connect_clicked(move |button| {
            button.set_sensitive(false);
            feedback_copy.set_text(if count.is_some() {
                "Disconnecting…"
            } else {
                "Connecting and reading available tools…"
            });
            let hub = hub_copy.clone();
            let id = id.clone();
            let cancel = CancellationToken::new();
            let (sender, mut receiver) = oneshot::channel();
            runtime_copy.spawn(async move {
                let result = if count.is_some() {
                    hub.disconnect(&id).await
                } else {
                    hub.connect(&id, cancel).await
                };
                let _ = sender.send(result);
            });
            let hub = hub_copy.clone();
            let runtime = runtime_copy.clone();
            let list = list_copy.clone();
            let feedback = feedback_copy.clone();
            let parent = parent_copy.clone();
            glib::timeout_add_local(Duration::from_millis(33), move || {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(oneshot::error::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(_) => Err(
                        "The connection worker stopped. Reopen Tool connections and try again."
                            .to_owned(),
                    ),
                };
                refresh(&list, &feedback, &parent, &hub, &runtime);
                feedback.set_text(
                    &result
                        .err()
                        .unwrap_or_else(|| "Tool connection updated.".into()),
                );
                glib::ControlFlow::Break
            });
        });
        let hub = hub.clone();
        let runtime = runtime.clone();
        let list = list.clone();
        let feedback = feedback.clone();
        let parent = parent.clone();
        remove.connect_clicked(move |_| {
            let confirm = Dialog::new("Remove tool connection?", &parent, 440);
            confirm.add_button("Keep connection", gtk::ResponseType::Cancel);
            confirm.add_button("Remove connection", gtk::ResponseType::Accept);
            confirm.set_default_response(gtk::ResponseType::Cancel);
            content(&confirm).append(&caption("Remove these connection settings? The external program and your conversations will stay saved."));
            let hub = hub.clone(); let runtime = runtime.clone(); let list = list.clone(); let feedback = feedback.clone(); let parent = parent.clone(); let id = config.id.clone();
            confirm.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept {
                    match hub.remove(&id) {
                        Ok(()) => { refresh(&list, &feedback, &parent, &hub, &runtime); feedback.set_text("Connection removed."); }
                        Err(error) => feedback.set_text(&error),
                    }
                }
                dialog.close();
            });
            confirm.present();
        });
    }
}

fn add_connection(
    parent: &gtk::Window,
    hub: &Arc<ToolHub>,
    runtime: &Arc<Runtime>,
    list: &gtk::Box,
    feedback: &gtk::Label,
    javascript: bool,
) {
    let dialog = Dialog::new(
        if javascript {
            "Add JavaScript tool plugin"
        } else {
            "Add MCP server"
        },
        parent,
        560,
    );
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Save connection", gtk::ResponseType::Accept);
    let area = content(&dialog);
    area.append(&caption(if javascript {
        "Runs a trusted server-side Harness plugin in an optional Node compatibility host. React panels and upstream agent-loop changes are not native tool plugins. Saving does not start the host."
    } else {
        "A local MCP server runs with your user account when you press Connect. Saving only records its launch settings."
    }));
    let id = entry(&area, "Connection ID", "");
    let default_runtime = "/usr/lib/DeepSeek Harness for Linux/runtime";
    let command = entry(
        &area,
        if javascript {
            "Harness runtime folder"
        } else {
            "Program"
        },
        if javascript { default_runtime } else { "" },
    );
    let arguments = if javascript {
        entry(&area, "JavaScript plugin path", "").upcast::<gtk::Widget>()
    } else {
        area.append(&caption("Arguments · one per line"));
        let text = gtk::TextView::new();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.update_property(&[gtk::accessible::Property::Label("Arguments")]);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&text)
            .min_content_height(100)
            .max_content_height(150)
            .propagate_natural_height(true)
            .build();
        area.append(&scroll);
        text.upcast::<gtk::Widget>()
    };
    let directory = entry(
        &area,
        if javascript {
            "Plugin settings file (optional)"
        } else {
            "Working folder (optional)"
        },
        "",
    );
    let environment = entry(&area, "Environment variable names (optional)", "");
    area.append(&caption("List names separated by commas. Values are read from the app's environment when connecting; do not enter passwords or API keys here."));
    let error = caption("");
    error.add_css_class("error");
    area.append(&error);
    let hub = hub.clone();
    let runtime = runtime.clone();
    let list = list.clone();
    let feedback = feedback.clone();
    let parent = parent.clone();
    dialog.connect_response(move |dialog, response| {
        if response != gtk::ResponseType::Accept {
            dialog.close();
            return;
        }
        let names = environment
            .text()
            .split([',', ' ', '\n'])
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let config = if javascript {
            javascript_config(
                hub.profile(),
                id.text().trim(),
                command.text().trim(),
                arguments
                    .downcast_ref::<gtk::Entry>()
                    .expect("plugin path")
                    .text()
                    .trim(),
                directory.text().trim(),
                names,
            )
        } else {
            let buffer = arguments
                .downcast_ref::<gtk::TextView>()
                .expect("arguments")
                .buffer();
            Ok(McpServerConfig {
                id: id.text().trim().to_owned(),
                command: command.text().trim().to_owned(),
                args: buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .lines()
                    .map(str::to_owned)
                    .collect(),
                cwd: (!directory.text().trim().is_empty())
                    .then(|| PathBuf::from(directory.text().trim())),
                env_names: names,
                timeout_secs: 60,
            })
        };
        match config.and_then(|config| hub.save(config)) {
            Ok(()) => {
                refresh(&list, &feedback, &parent, &hub, &runtime);
                feedback
                    .set_text("Connection saved. Press Connect when you are ready to start it.");
                dialog.close();
            }
            Err(message) => error.set_text(&message),
        }
    });
    dialog.present();
}

fn javascript_config(
    profile: &Path,
    id: &str,
    runtime: &str,
    plugin: &str,
    config_file: &str,
    env_names: Vec<String>,
) -> Result<McpServerConfig, String> {
    let runtime = PathBuf::from(runtime);
    let plugin = PathBuf::from(plugin);
    if !runtime.is_absolute()
        || !runtime
            .join("node_modules/@deepseek-ai/dsh/package.json")
            .is_file()
        || !plugin.is_absolute()
        || !plugin.exists()
    {
        return Err(
            "Choose an installed Harness runtime and an existing absolute plugin path.".into(),
        );
    }
    let packaged = PathBuf::from("/usr/share/deepseek-harness-native/compat/host.mjs");
    let host = if packaged.is_file() {
        packaged
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/host.mjs")
    };
    if !host.is_file() {
        return Err(
            "The JavaScript compatibility host is missing. Reinstall the native package.".into(),
        );
    }
    let bundled_node = runtime.join("node");
    let node = if bundled_node.is_file() {
        bundled_node
    } else {
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|path| path.join("node"))
                    .find(|path| path.is_file())
            })
            .ok_or("Install Node or select a Harness runtime containing its Node executable.")?
    };
    let mut config = McpServerConfig {
        id: id.to_owned(),
        command: node.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: None,
        env_names,
        timeout_secs: 60,
    };
    config.validate().map_err(|error| error.to_string())?;
    config.args = vec![
        host.to_string_lossy().into_owned(),
        "--runtime".into(),
        runtime.to_string_lossy().into_owned(),
        "--profile".into(),
        profile
            .join("compat")
            .join(id)
            .to_string_lossy()
            .into_owned(),
        "--plugin".into(),
        plugin.to_string_lossy().into_owned(),
    ];
    if !config_file.is_empty() {
        if !Path::new(config_file).is_absolute() || !Path::new(config_file).is_file() {
            return Err("Choose an existing absolute plugin settings file path.".into());
        }
        config
            .args
            .extend(["--plugin-config-file".into(), config_file.into()]);
    }
    for name in &config.env_names {
        config.args.extend(["--env-name".into(), name.clone()]);
    }
    Ok(config)
}
