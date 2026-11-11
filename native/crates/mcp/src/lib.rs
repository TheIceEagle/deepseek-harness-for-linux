//! Local MCP tools for the native app. The app approves commands and tool calls.
//!
//! This transport grants no sampling, roots, or elicitation capabilities. Server
//! instructions are not promoted into the model's system prompt. Connections use
//! private pipes; child stderr is discarded and public errors contain no peer
//! payloads. Keep SDK protocol tracing disabled when handling private tool data.

use rmcp::{
    model::{CallToolRequestParams, ClientConfig, Implementation, PaginatedRequestParams},
    service::{Peer, RunningService},
    transport::async_rw::AsyncRwTransport,
    RoleClient, ServiceExt,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fmt,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{process::Child, sync::Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

pub const MAX_TOOLS: usize = 64;
pub const MAX_TOOL_TEXT_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 256 * 1024;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Names of explicitly approved ambient variables, never their values.
    pub env_names: Vec<String>,
    pub timeout_secs: u64,
}

impl fmt::Debug for McpServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerConfig")
            .field("id", &self.id)
            .field("arguments", &self.args.len())
            .field("environment_names", &self.env_names.len())
            .field("timeout_secs", &self.timeout_secs)
            .finish_non_exhaustive()
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            env_names: Vec::new(),
            timeout_secs: 60,
        }
    }
}

impl McpServerConfig {
    pub fn validate(&self) -> Result<(), McpError> {
        let identifier = !self.id.is_empty()
            && self.id.len() <= 24
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
        let command = !self.command.trim().is_empty()
            && self.command.len() <= 4096
            && !self.command.contains('\0')
            && (!self.command.contains('/') || PathBuf::from(&self.command).is_absolute());
        let arguments = self.args.len() <= 64
            && self
                .args
                .iter()
                .all(|arg| arg.len() <= 4096 && !arg.contains('\0'))
            && self.args.iter().map(String::len).sum::<usize>() <= MAX_TOOL_TEXT_BYTES;
        let environment = self.env_names.len() <= 32
            && self.env_names.iter().all(|name| {
                !name.is_empty()
                    && name.len() <= 128
                    && name != "DEEPSEEK_API_KEY"
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && !name.as_bytes()[0].is_ascii_digit()
            });
        let cwd = self.cwd.as_ref().is_none_or(|path| {
            path.is_absolute()
                && path.as_os_str().as_encoded_bytes().len() <= 4096
                && !path.as_os_str().as_encoded_bytes().contains(&0)
        });
        if !identifier
            || !command
            || !arguments
            || !environment
            || !cwd
            || !(1..=300).contains(&self.timeout_secs)
        {
            return Err(McpError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpTool {
    /// Stable server-qualified name presented to the model; never a wire name.
    pub name: String,
    pub original_name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpError {
    InvalidConfiguration,
    StartFailed,
    ConnectionFailed,
    Cancelled,
    Timeout,
    InvalidTools,
    TooManyTools,
    UnknownTool,
    InvalidArguments,
    ToolFailed,
    UnsupportedContent,
    OutputTooLarge,
    Disconnected,
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "Check the server identifier, executable, arguments, environment names, folder, and timeout.",
            Self::StartFailed => "The MCP server could not start. Check its executable and working folder.",
            Self::ConnectionFailed => "The MCP server did not establish a compatible tool connection.",
            Self::Cancelled => "The MCP operation was stopped.",
            Self::Timeout => "The MCP server exceeded its time limit. Reconnect before trying again.",
            Self::InvalidTools => "The MCP server returned an invalid or oversized tool catalog.",
            Self::TooManyTools => "The MCP server exceeds the native bridge's 64-tool limit.",
            Self::UnknownTool => "This tool is not available from the connected MCP server.",
            Self::InvalidArguments => "The tool arguments must be a JSON object of at most 64 KiB.",
            Self::ToolFailed => "The MCP tool reported a failure. Review its inputs before trying again.",
            Self::UnsupportedContent => "This MCP tool requires image, audio, resource, or interactive results that the text bridge does not support yet.",
            Self::OutputTooLarge => "The MCP tool returned more than 64 KiB of text.",
            Self::Disconnected => "The MCP connection has stopped. Reconnect before trying again.",
        })
    }
}

impl std::error::Error for McpError {}

type Service = RunningService<RoleClient, ClientConfig>;

struct Inner {
    peer: Peer<RoleClient>,
    service: AsyncMutex<Option<Service>>,
    closing: AsyncMutex<()>,
    process: Mutex<OwnedProcess>,
    lifetime: CancellationToken,
    timeout: Duration,
    tools: Vec<McpTool>,
}

/// Clone handles share one connection. `close` disconnects every clone.
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("connected", &self.is_connected())
            .field("tools", &self.tools().len())
            .finish_non_exhaustive()
    }
}

impl McpClient {
    pub async fn connect(
        config: &McpServerConfig,
        cancel: CancellationToken,
    ) -> Result<Self, McpError> {
        config.validate()?;
        if cancel.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        // Moved working folders must not invalidate saved settings at startup.
        // Runtime availability is checked only on the user's explicit Connect.
        if config.cwd.as_ref().is_some_and(|path| !path.is_dir()) {
            return Err(McpError::InvalidConfiguration);
        }
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        for name in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for name in &config.env_names {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        install_parent_death_guard(&mut command)?;
        let mut child = command.spawn().map_err(|_| McpError::StartFailed)?;
        let output = child.stdout.take().ok_or(McpError::StartFailed)?;
        let input = child.stdin.take().ok_or(McpError::StartFailed)?;
        let process = OwnedProcess(Some(child));
        let transport = AsyncRwTransport::<RoleClient, _, _>::new_client(output, input)
            .with_max_line_length(MAX_LINE_BYTES);
        let lifetime = CancellationToken::new();
        let info = ClientConfig::new(
            Default::default(),
            Implementation::new("deepseek-harness-native", env!("CARGO_PKG_VERSION")),
        );
        let timeout = Duration::from_secs(config.timeout_secs);
        let startup = async {
            let service = info
                .serve_with_ct(transport, lifetime.clone())
                .await
                .map_err(|_| McpError::ConnectionFailed)?;
            let tools = discover(service.peer(), &config.id).await?;
            Ok::<_, McpError>((service, tools))
        };
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(McpError::Cancelled),
            result = tokio::time::timeout(timeout, startup) => result.map_err(|_| McpError::Timeout)?,
        };
        let (service, tools) = result?;
        Ok(Self {
            inner: Arc::new(Inner {
                peer: service.peer().clone(),
                service: AsyncMutex::new(Some(service)),
                closing: AsyncMutex::new(()),
                process: Mutex::new(process),
                lifetime,
                timeout,
                tools,
            }),
        })
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.inner.tools
    }

    pub fn is_connected(&self) -> bool {
        !self.inner.lifetime.is_cancelled() && !self.inner.peer.is_transport_closed()
    }

    pub async fn call(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<String, McpError> {
        if !self.is_connected() {
            return Err(McpError::Disconnected);
        }
        let tool = self
            .inner
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .ok_or(McpError::UnknownTool)?;
        let bytes = serde_json::to_vec(&arguments).map_err(|_| McpError::InvalidArguments)?;
        if !arguments.is_object() || bytes.len() > MAX_TOOL_TEXT_BYTES {
            return Err(McpError::InvalidArguments);
        }
        let request = CallToolRequestParams::new(tool.original_name.clone())
            .with_arguments(arguments.as_object().expect("checked object").clone());
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(McpError::Cancelled),
            () = self.inner.lifetime.cancelled() => Err(McpError::Disconnected),
            result = tokio::time::timeout(self.inner.timeout, self.inner.peer.call_tool_once(request)) => {
                result.map_err(|_| McpError::Timeout).and_then(|result| result.map_err(|_| McpError::ToolFailed))
            }
        };
        // Stop an uncooperative server and its owned processes rather than leave
        // a timed-out or cancelled operation silently running in the background.
        if matches!(result, Err(McpError::Cancelled | McpError::Timeout)) {
            self.close().await;
        }
        let result = result?;
        let rmcp::model::CallToolResponse::Complete(result) = result else {
            self.close().await;
            return Err(McpError::UnsupportedContent);
        };
        if result.is_error == Some(true) {
            return Err(McpError::ToolFailed);
        }
        let mut output = String::new();
        for content in result.content {
            let Some(text) = content.as_text() else {
                return Err(McpError::UnsupportedContent);
            };
            append_output(&mut output, &text.text)?;
        }
        if let Some(structured) = result.structured_content {
            append_output(
                &mut output,
                &serde_json::to_string(&structured).map_err(|_| McpError::UnsupportedContent)?,
            )?;
        }
        Ok(output)
    }

    /// Close pipes, stop the owned process group, and reap the server before returning.
    pub async fn close(&self) {
        // Every clone waits for the same shutdown, including child reaping.
        let _closing = self.inner.closing.lock().await;
        self.inner.lifetime.cancel();
        if let Some(mut service) = self.inner.service.lock().await.take() {
            let _ = service.close_with_timeout(Duration::from_millis(300)).await;
        }
        let process = self
            .inner
            .process
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0
            .take();
        if let Some(mut child) = process {
            kill_group(&child);
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

fn install_parent_death_guard(command: &mut tokio::process::Command) -> Result<(), McpError> {
    #[cfg(target_os = "linux")]
    {
        let parent = i32::try_from(std::process::id()).map_err(|_| McpError::StartFailed)?;
        // SAFETY: the closure runs between fork and exec and uses only Linux
        // syscalls plus construction of an OS error. No locks or allocations
        // involving application state are accessed in the child.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The parent can die after fork but before prctl installs the
                // signal. Refuse to start an orphaned server in that interval.
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
    Ok(())
}

fn append_output(output: &mut String, text: &str) -> Result<(), McpError> {
    let extra = usize::from(!output.is_empty());
    if output
        .len()
        .saturating_add(text.len())
        .saturating_add(extra)
        > MAX_TOOL_TEXT_BYTES
    {
        return Err(McpError::OutputTooLarge);
    }
    if extra > 0 {
        output.push('\n');
    }
    output.push_str(text);
    Ok(())
}

async fn discover(peer: &Peer<RoleClient>, server: &str) -> Result<Vec<McpTool>, McpError> {
    let mut tools = Vec::new();
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_names = HashSet::new();
    let mut names = HashSet::new();
    let mut bytes = 0usize;
    for _ in 0..=MAX_TOOLS {
        let params = cursor
            .take()
            .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
        let page = peer
            .list_tools(params)
            .await
            .map_err(|_| McpError::ConnectionFailed)?;
        for tool in page.tools {
            if tools.len() >= MAX_TOOLS {
                return Err(McpError::TooManyTools);
            }
            let original_name = tool.name.into_owned();
            let description = tool
                .description
                .map(|value| value.into_owned())
                .unwrap_or_default();
            let parameters = Value::Object(tool.input_schema.as_ref().clone());
            if original_name.is_empty()
                || original_name.len() > 128
                || original_name.chars().any(char::is_control)
                || description.len() > 4096
                || parameters.get("type").and_then(Value::as_str) != Some("object")
                || !seen_names.insert(original_name.clone())
            {
                return Err(McpError::InvalidTools);
            }
            let encoded = serde_json::to_vec(&parameters).map_err(|_| McpError::InvalidTools)?;
            bytes = bytes
                .saturating_add(encoded.len())
                .saturating_add(description.len())
                .saturating_add(original_name.len());
            if encoded.len() > 16 * 1024 || bytes > MAX_CATALOG_BYTES {
                return Err(McpError::InvalidTools);
            }
            let name = public_name(server, &original_name);
            if !names.insert(name.clone()) {
                return Err(McpError::InvalidTools);
            }
            tools.push(McpTool {
                name,
                original_name,
                description,
                parameters,
            });
        }
        match page.next_cursor {
            Some(next) if next.len() <= 1024 && seen_cursors.insert(next.clone()) => {
                cursor = Some(next)
            }
            Some(_) => return Err(McpError::InvalidTools),
            None => {
                tools.sort_by(|a, b| a.name.cmp(&b.name));
                return Ok(tools);
            }
        }
    }
    Err(McpError::InvalidTools)
}

fn public_name(server: &str, raw: &str) -> String {
    // Deterministic identity, independent of discovery order. A collision rejects
    // the catalog; public names are always mapped back through the frozen catalog.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in raw.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let label: String = raw
        .bytes()
        .take(16)
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect();
    format!("mcp__{server}__{label}_{hash:016x}")
}

struct OwnedProcess(Option<Child>);

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            kill_group(&child);
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

fn kill_group(child: &Child) {
    if let Some(pid) = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    {
        // SAFETY: this PID belongs to an unreaped child spawned with its own
        // process group. Keeping Child until after this call prevents PID reuse.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_names_are_valid_and_do_not_alias_sanitized_tools() {
        let first = public_name("browser", "tabs.read");
        assert_eq!(first, public_name("browser", "tabs.read"));
        assert_ne!(first, public_name("browser", "tabs_read"));
        assert!(first.len() <= 64);
        assert!(first
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    }

    #[test]
    fn validates_commands_and_never_allows_the_model_key_environment() {
        let valid = McpServerConfig {
            id: "browser".into(),
            command: "npx".into(),
            ..Default::default()
        };
        assert!(valid.validate().is_ok());
        for value in [
            McpServerConfig {
                id: "../escape".into(),
                ..valid.clone()
            },
            McpServerConfig {
                env_names: vec!["DEEPSEEK_API_KEY".into()],
                ..valid.clone()
            },
            McpServerConfig {
                command: "./script".into(),
                ..valid.clone()
            },
            McpServerConfig {
                timeout_secs: 0,
                ..valid.clone()
            },
        ] {
            assert_eq!(value.validate(), Err(McpError::InvalidConfiguration));
        }
    }

    #[test]
    fn configuration_debug_does_not_print_program_arguments_or_environment() {
        let config = McpServerConfig {
            id: "browser".into(),
            command: "synthetic-private-command".into(),
            args: vec!["synthetic-private-argument".into()],
            env_names: vec!["SYNTHETIC_PRIVATE_ENV".into()],
            ..Default::default()
        };
        let diagnostic = format!("{config:?}");
        for marker in [
            "synthetic-private-command",
            "synthetic-private-argument",
            "SYNTHETIC_PRIVATE_ENV",
        ] {
            assert!(!diagnostic.contains(marker));
        }
    }
}
