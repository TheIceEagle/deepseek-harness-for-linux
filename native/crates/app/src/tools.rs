//! Opt-in external tools and bounded WASM commands share one per-turn catalog.

use harness_native_core::{ToolCall, ToolDefinition};
use harness_native_mcp::{McpClient, McpServerConfig};
use harness_native_plugins::PluginManager;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connections {
    servers: Vec<McpServerConfig>,
    #[serde(default)]
    wasm_tools_enabled: bool,
}

struct State {
    config: Connections,
    clients: HashMap<String, McpClient>,
    connecting: HashMap<String, CancellationToken>,
    closing: bool,
}

pub struct ToolHub {
    profile: PathBuf,
    plugins: PluginManager,
    state: Mutex<State>,
    quiescent: Notify,
}

enum Route {
    Mcp(McpClient),
    Wasm(PluginManager, String),
}

/// Frozen until this model turn ends: a changing connection cannot reroute a call.
pub struct ToolSet {
    pub definitions: Vec<ToolDefinition>,
    routes: HashMap<String, Route>,
}

impl ToolHub {
    pub fn open(profile: &Path, plugins: PluginManager) -> Result<Arc<Self>, String> {
        let config = match File::open(profile.join("tool-connections.json")) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CONFIG_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Tool connections could not be read.")?;
                if bytes.len() as u64 > MAX_CONFIG_BYTES {
                    return Err("The tool connection file is too large.".into());
                }
                serde_json::from_slice(&bytes).map_err(|_| {
                    "Tool connections are invalid. Keep the file intact and restore a backup."
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Connections::default(),
            Err(_) => return Err("Tool connections could not be read.".into()),
        };
        validate_connections(&config)?;
        Ok(Arc::new(Self {
            profile: profile.to_owned(),
            plugins,
            quiescent: Notify::new(),
            state: Mutex::new(State {
                config,
                clients: HashMap::new(),
                connecting: HashMap::new(),
                closing: false,
            }),
        }))
    }

    pub fn connections(&self) -> Vec<(McpServerConfig, Option<usize>)> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .config
            .servers
            .iter()
            .map(|server| {
                (
                    server.clone(),
                    state
                        .clients
                        .get(&server.id)
                        .filter(|client| client.is_connected())
                        .map(|client| client.tools().len()),
                )
            })
            .collect()
    }

    pub fn save(&self, config: McpServerConfig) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tool connections are unavailable.")?;
        if state.clients.contains_key(&config.id) || state.connecting.contains_key(&config.id) {
            return Err("Disconnect this connection before changing its settings.".into());
        }
        let mut servers = state.config.servers.clone();
        if servers.iter().any(|entry| entry.id == config.id) {
            return Err(
                "A connection with this ID already exists. Remove it before adding another.".into(),
            );
        }
        servers.push(config);
        let next = Connections {
            servers,
            wasm_tools_enabled: state.config.wasm_tools_enabled,
        };
        validate_connections(&next)?;
        self.persist(&next)?;
        state.config = next;
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tool connections are unavailable.")?;
        if state
            .clients
            .get(id)
            .is_some_and(|client| client.is_connected())
            || state.connecting.contains_key(id)
        {
            return Err("Disconnect this connection before removing it.".into());
        }
        let next = Connections {
            wasm_tools_enabled: state.config.wasm_tools_enabled,
            servers: state
                .config
                .servers
                .iter()
                .filter(|entry| entry.id != id)
                .cloned()
                .collect(),
        };
        self.persist(&next)?;
        state.clients.remove(id);
        state.config = next;
        Ok(())
    }

    pub async fn connect(&self, id: &str, cancel: CancellationToken) -> Result<(), String> {
        let config = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Tool connections are unavailable.")?;
            if state.closing {
                return Err("The app is closing its tool connections.".into());
            }
            if state
                .clients
                .get(id)
                .is_some_and(|client| !client.is_connected())
            {
                state.clients.remove(id);
            }
            if state.clients.contains_key(id) || state.connecting.contains_key(id) {
                return Err("This connection is already running or connecting.".into());
            }
            let config = state
                .config
                .servers
                .iter()
                .find(|entry| entry.id == id)
                .cloned()
                .ok_or("This connection no longer exists.")?;
            state.connecting.insert(id.to_owned(), cancel.clone());
            config
        };
        match McpClient::connect(&config, cancel.clone()).await {
            Ok(client) => {
                let stop = {
                    let mut state = self
                        .state
                        .lock()
                        .map_err(|_| "Tool connections are unavailable.")?;
                    if state.closing || cancel.is_cancelled() {
                        true
                    } else {
                        state.clients.insert(id.to_owned(), client.clone());
                        state.connecting.remove(id);
                        false
                    }
                };
                if stop {
                    client.close().await;
                    self.state
                        .lock()
                        .map_err(|_| "Tool connections are unavailable.")?
                        .connecting
                        .remove(id);
                }
                self.quiescent.notify_waiters();
                if stop {
                    Err("The connection was stopped.".into())
                } else {
                    Ok(())
                }
            }
            Err(error) => {
                self.state
                    .lock()
                    .map_err(|_| "Tool connections are unavailable.")?
                    .connecting
                    .remove(id);
                self.quiescent.notify_waiters();
                Err(error.to_string())
            }
        }
    }

    pub async fn disconnect(&self, id: &str) -> Result<(), String> {
        let client = self
            .state
            .lock()
            .map_err(|_| "Tool connections are unavailable.")?
            .clients
            .remove(id);
        if let Some(client) = client {
            client.close().await;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Arc<ToolSet>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Tool connections are unavailable.")?;
        let mut definitions = Vec::new();
        let mut routes = HashMap::new();
        for server in &state.config.servers {
            if let Some(client) = state
                .clients
                .get(&server.id)
                .filter(|client| client.is_connected())
            {
                for tool in client.tools() {
                    if routes
                        .insert(tool.name.clone(), Route::Mcp(client.clone()))
                        .is_some()
                    {
                        return Err(
                            "Tool names conflict. Disconnect the conflicting connection.".into(),
                        );
                    }
                    definitions.push(ToolDefinition {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.parameters.clone(),
                    });
                }
            }
        }
        let plugins = if state.config.wasm_tools_enabled {
            self.plugins.list().map_err(|error| error.to_string())?
        } else {
            Vec::new()
        };
        for plugin in plugins {
            let name = wasm_name(&plugin.id);
            if routes
                .insert(name.clone(), Route::Wasm(self.plugins.clone(), plugin.id))
                .is_some()
            {
                return Err("Extension tool names conflict.".into());
            }
            definitions.push(ToolDefinition {
                name, description: format!("{}: {}. Returns transformed text; does not change the composer.", plugin.command, plugin.description),
                input_schema: json!({"type":"object","properties":{"text":{"type":"string","description":"Text to transform","maxLength":65536}},"required":["text"],"additionalProperties":false}),
            });
        }
        if definitions.len() > 64 {
            return Err(
                "Too many tools are connected. Disconnect a server to use at most 64 tools.".into(),
            );
        }
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Arc::new(ToolSet {
            definitions,
            routes,
        }))
    }

    pub fn profile(&self) -> &Path {
        &self.profile
    }

    pub fn wasm_enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .config
            .wasm_tools_enabled
    }

    pub fn enable_wasm(&self, enabled: bool) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tool connections are unavailable.")?;
        let next = Connections {
            servers: state.config.servers.clone(),
            wasm_tools_enabled: enabled,
        };
        self.persist(&next)?;
        state.config = next;
        Ok(())
    }

    pub async fn close_all(&self) {
        let clients = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.closing = true;
            for cancel in state.connecting.values() {
                cancel.cancel();
            }
            state
                .clients
                .drain()
                .map(|(_, client)| client)
                .collect::<Vec<_>>()
        };
        let mut closing = tokio::task::JoinSet::new();
        for client in clients {
            closing.spawn(async move {
                client.close().await;
            });
        }
        while closing.join_next().await.is_some() {}
        loop {
            let changed = self.quiescent.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .connecting
                .is_empty()
            {
                break;
            }
            changed.await;
        }
    }

    fn persist(&self, config: &Connections) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|_| "Tool connections could not be saved.")?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err("The tool connection file is too large.".into());
        }
        let temporary = self.profile.join(format!(
            ".tools-{}-{}.tmp",
            std::process::id(),
            NEXT_WRITE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            fs::rename(&temporary, self.profile.join("tool-connections.json"))?;
            File::open(&self.profile)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(
                "Tool connections could not be saved. Check folder permissions and free space."
                    .into(),
            );
        }
        Ok(())
    }
}

impl ToolSet {
    pub async fn execute(
        &self,
        call: &ToolCall,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        if cancel.is_cancelled() {
            return Err("Tool execution was stopped.".into());
        }
        match self.routes.get(&call.name) {
            Some(Route::Mcp(client)) => client
                .call(&call.name, call.arguments.clone(), cancel)
                .await
                .map_err(|error| error.to_string()),
            Some(Route::Wasm(manager, id)) => {
                let object = call
                    .arguments
                    .as_object()
                    .ok_or("The extension requires a text argument.")?;
                if object.len() != 1 {
                    return Err("The extension accepts only a text argument.".into());
                }
                let text = object
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("The extension requires a text argument.")?
                    .to_owned();
                let manager = manager.clone();
                let id = id.clone();
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => Err("Tool execution was stopped.".into()),
                    result = tokio::task::spawn_blocking(move || manager.run(&id, &text)) => result.map_err(|_| "The extension worker stopped.".to_owned())?.map_err(|error| error.to_string()),
                }
            }
            None => Err("This tool is not part of the approved conversation's catalog.".into()),
        }
    }
}

fn validate_connections(config: &Connections) -> Result<(), String> {
    if config.servers.len() > 16 {
        return Err("Use at most 16 tool connections.".into());
    }
    let mut ids = HashSet::new();
    for server in &config.servers {
        server.validate().map_err(|error| error.to_string())?;
        if !ids.insert(&server.id) {
            return Err("Tool connection IDs must be unique.".into());
        }
    }
    Ok(())
}

fn wasm_name(id: &str) -> String {
    if id.len() <= 58 {
        return format!("wasm__{id}");
    }
    let hash = id.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("wasm__{}_{hash:016x}", &id[..40])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            command: "python3".into(),
            ..Default::default()
        }
    }

    #[test]
    fn configurations_are_private_and_never_start_on_open() {
        let profile = tempfile::tempdir().unwrap();
        let manager = PluginManager::new(profile.path()).unwrap();
        let hub = ToolHub::open(profile.path(), manager.clone()).unwrap();
        hub.save(config("local")).unwrap();
        assert!(hub.save(config("local")).is_err());
        let path = profile.path().join("tool-connections.json");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let reopened = ToolHub::open(profile.path(), manager).unwrap();
        assert_eq!(reopened.connections().len(), 1);
        assert_eq!(reopened.connections()[0].1, None);
        assert!(reopened.snapshot().unwrap().definitions.is_empty());
        reopened.remove("local").unwrap();
        assert!(reopened.connections().is_empty());
    }

    #[test]
    fn failed_configuration_save_keeps_previous_connections() {
        let profile = tempfile::tempdir().unwrap();
        let hub =
            ToolHub::open(profile.path(), PluginManager::new(profile.path()).unwrap()).unwrap();
        hub.save(config("first")).unwrap();
        let path = profile.path().join("tool-connections.json");
        fs::rename(&path, profile.path().join("backup")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(hub.save(config("second")).is_err());
        assert_eq!(hub.connections().len(), 1);
        assert_eq!(hub.connections()[0].0.id, "first");
    }

    #[tokio::test]
    async fn quitting_cancels_and_awaits_in_flight_server_startup() {
        let profile = tempfile::tempdir().unwrap();
        let hub =
            ToolHub::open(profile.path(), PluginManager::new(profile.path()).unwrap()).unwrap();
        let marker = profile.path().join("server.pid");
        let mut config = config("starting");
        config.args = vec![
            "-c".into(),
            "import os,sys,time; open(sys.argv[1], 'w').write(str(os.getpid())); time.sleep(60)"
                .into(),
            marker.to_string_lossy().into_owned(),
        ];
        hub.save(config).unwrap();
        let connecting = hub.clone();
        let task = tokio::spawn(async move {
            connecting
                .connect("starting", CancellationToken::new())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), hub.close_all())
            .await
            .unwrap();
        assert!(task.await.unwrap().is_err());
        assert!(hub
            .connect("starting", CancellationToken::new())
            .await
            .is_err());
        assert_eq!(hub.connections()[0].1, None);
        hub.remove("starting").unwrap();
        assert!(hub.connections().is_empty());
    }

    #[tokio::test]
    async fn installed_wasm_extension_is_a_frozen_model_tool() {
        let folder = tempfile::tempdir().unwrap();
        let profile = folder.path().join("profile");
        let source = folder.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            include_str!("../../../examples/uppercase/plugin.json"),
        )
        .unwrap();
        fs::write(
            source.join("plugin.wasm"),
            wat::parse_str(include_str!("../../../examples/uppercase/plugin.wat")).unwrap(),
        )
        .unwrap();
        let manager = PluginManager::new(&profile).unwrap();
        manager.install(source).unwrap();
        let hub = ToolHub::open(&profile, manager).unwrap();
        assert!(hub.snapshot().unwrap().definitions.is_empty());
        hub.enable_wasm(true).unwrap();
        let tools = hub.snapshot().unwrap();
        assert_eq!(tools.definitions.len(), 1);
        let mut call = ToolCall {
            id: "call-1".into(),
            name: "wasm__uppercase".into(),
            arguments: json!({"text":"hello café"}),
        };
        assert_eq!(
            tools
                .execute(&call, CancellationToken::new())
                .await
                .unwrap(),
            "HELLO CAFé"
        );
        call.arguments = json!({"text":"hello", "extra":true});
        assert!(tools
            .execute(&call, CancellationToken::new())
            .await
            .is_err());
        call.name = "not-connected".into();
        assert!(tools
            .execute(&call, CancellationToken::new())
            .await
            .is_err());
    }
    #[test]
    fn wasm_names_stay_stable_and_fit_the_provider_contract() {
        assert_eq!(wasm_name("uppercase"), "wasm__uppercase");
        let id = "a".repeat(64);
        assert_eq!(wasm_name(&id), wasm_name(&id));
        assert!(wasm_name(&id).len() <= 64);
        assert_ne!(wasm_name(&id), wasm_name(&format!("{}b", "a".repeat(63))));
    }
}
