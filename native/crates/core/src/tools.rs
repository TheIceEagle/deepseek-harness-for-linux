use crate::CoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const MAX_TOOLS: usize = 64;
pub const MAX_TOOL_CALLS: usize = 8;
pub const MAX_TOOL_ROUNDS: usize = 8;
pub const MAX_TOOL_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Canonical provider messages beneath one visible assistant reply. A terminal
/// assistant round has no calls/results; visible text may join several rounds.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ToolRound {
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning_content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<ToolResult>,
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn validate_definitions(tools: &[ToolDefinition]) -> Result<(), CoreError> {
    let mut names = HashSet::new();
    if tools.len() > MAX_TOOLS
        || tools.iter().any(|tool| {
            !valid_name(&tool.name)
                || !names.insert(&tool.name)
                || tool.description.len() > 4096
                || !tool.input_schema.is_object()
                || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
                || tool.input_schema.to_string().len() > MAX_TOOL_TEXT_BYTES
        })
    {
        return Err(CoreError::new("The connected tool definitions are invalid or exceed the current limits. Disconnect the server and check its configuration."));
    }
    Ok(())
}

pub(crate) fn validate_trace(trace: &[ToolRound]) -> Result<(), CoreError> {
    let mut ids = HashSet::new();
    if trace.len() > MAX_TOOL_ROUNDS + 1 {
        return Err(trace_error());
    }
    for round in trace {
        if round.content.len() > 2 * 1024 * 1024
            || round.reasoning_content.len() > 2 * 1024 * 1024
            || round.tool_calls.len() > MAX_TOOL_CALLS
            || round.results.len() > round.tool_calls.len()
        {
            return Err(trace_error());
        }
        for call in &round.tool_calls {
            if call.id.is_empty()
                || call.id.len() > 256
                || !ids.insert(&call.id)
                || !valid_name(&call.name)
                || !call.arguments.is_object()
                || call.arguments.to_string().len() > MAX_TOOL_TEXT_BYTES
            {
                return Err(trace_error());
            }
        }
        let mut results = HashSet::new();
        for result in &round.results {
            if !round
                .tool_calls
                .iter()
                .any(|call| call.id == result.tool_call_id)
                || !results.insert(&result.tool_call_id)
                || result.content.len() > MAX_TOOL_TEXT_BYTES
            {
                return Err(trace_error());
            }
        }
    }
    Ok(())
}

pub(crate) fn recover_trace(trace: &mut [ToolRound]) -> bool {
    let mut recovered = false;
    for round in trace {
        for call in &round.tool_calls {
            if !round
                .results
                .iter()
                .any(|result| result.tool_call_id == call.id)
            {
                round.results.push(ToolResult {
                    tool_call_id: call.id.clone(),
                    content: "The app was interrupted before this tool result was saved. The tool may have executed; its outcome is unknown. Do not repeat it without explicit new approval.".into(),
                    is_error: true,
                });
                recovered = true;
            }
        }
    }
    recovered
}

fn trace_error() -> CoreError {
    CoreError::new("The saved tool conversation is invalid or exceeds the current limits. Keep the profile intact and restore it from a backup.")
}
