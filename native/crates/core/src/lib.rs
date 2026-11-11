//! Native chat state and an OpenAI-compatible streaming client.
//!
//! API credentials are passed to a single request in memory. They are never part
//! of serializable settings, persistent state, error messages, or diagnostics.

mod agent;
mod provider;
mod store;
mod tools;

pub use agent::{AgentEvent, AgentOutcome, ToolDecision};
pub use provider::{Provider, StreamEvent, StreamOutcome};
pub use store::{Message, MessageStatus, Role, Session, Settings, Store};
pub use tools::{
    ToolCall, ToolDefinition, ToolResult, ToolRound, MAX_TOOLS, MAX_TOOL_CALLS, MAX_TOOL_ROUNDS,
    MAX_TOOL_TEXT_BYTES,
};

/// A user-facing failure that never includes a provider body, URL, or credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreError(&'static str);

impl CoreError {
    pub(crate) const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CoreError {}
