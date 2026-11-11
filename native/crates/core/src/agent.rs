use crate::tools::{validate_definitions, MAX_TOOL_ROUNDS, MAX_TOOL_TEXT_BYTES};
use crate::{
    CoreError, Message, Provider, Settings, StreamOutcome, ToolCall, ToolDefinition, ToolResult,
    ToolRound,
};
use std::{collections::HashSet, time::Duration};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// The caller owns both user approval and tool execution. The core never runs
/// a tool itself or treats an MCP annotation as authorization.
#[derive(Debug)]
pub enum ToolDecision {
    Result(String),
    Denied,
    Failed(String),
}

#[derive(Debug)]
pub enum AgentEvent {
    Delta(String),
    /// Acknowledge only after atomically saving the trace and visible reply.
    Checkpoint {
        trace: Vec<ToolRound>,
        ack: oneshot::Sender<bool>,
    },
    /// The preceding plan checkpoint has already been acknowledged as durable.
    ToolRequest {
        call: ToolCall,
        reply: oneshot::Sender<ToolDecision>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentOutcome {
    pub outcome: StreamOutcome,
    pub trace: Vec<ToolRound>,
}

impl Provider {
    pub async fn run_agent(
        &self,
        settings: &Settings,
        api_key: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        sender: mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) -> Result<AgentOutcome, CoreError> {
        validate_definitions(tools)?;
        let mut trace = Vec::new();
        let mut seen = HashSet::new();
        let mut has_visible_text = false;
        let mut visible_bytes = 0usize;
        for round_index in 0..=MAX_TOOL_ROUNDS {
            if cancel.is_cancelled() {
                return Ok(AgentOutcome {
                    outcome: StreamOutcome::Cancelled,
                    trace,
                });
            }
            if has_visible_text {
                visible_bytes = visible_bytes.saturating_add(2);
                if visible_bytes > crate::provider::MAX_RESPONSE_BYTES {
                    return Err(CoreError::new("The response exceeded the native client's current size limit. The partial response has been kept."));
                }
            }
            if has_visible_text && !emit(&sender, AgentEvent::Delta("\n\n".into()), &cancel).await {
                return Ok(AgentOutcome {
                    outcome: StreamOutcome::Cancelled,
                    trace,
                });
            }
            let turn = self
                .stream_turn(
                    settings,
                    api_key,
                    messages,
                    &trace,
                    tools,
                    &sender,
                    &cancel,
                    crate::provider::MAX_RESPONSE_BYTES - visible_bytes,
                )
                .await?;
            has_visible_text = !turn.round.content.is_empty();
            visible_bytes += turn.round.content.len();
            if turn.cancelled {
                if !trace.is_empty() && !turn.round.content.is_empty() {
                    trace.push(turn.round);
                }
                return Ok(AgentOutcome {
                    outcome: StreamOutcome::Cancelled,
                    trace,
                });
            }
            let calls = turn.round.tool_calls.clone();
            if calls.is_empty() {
                if !trace.is_empty()
                    || !tools.is_empty()
                    || !turn.round.reasoning_content.is_empty()
                {
                    trace.push(turn.round);
                    if !checkpoint(&sender, &trace, &cancel).await? {
                        return Ok(AgentOutcome {
                            outcome: StreamOutcome::Cancelled,
                            trace,
                        });
                    }
                }
                return Ok(AgentOutcome {
                    outcome: StreamOutcome::Completed,
                    trace,
                });
            }
            if round_index == MAX_TOOL_ROUNDS {
                return Err(CoreError::new("The reply reached its tool-round limit. Review the saved work before continuing."));
            }
            if calls.iter().any(|call| !seen.insert(call.id.clone())) {
                return Err(CoreError::new(
                    "The provider repeated a tool call identifier. No repeated tool was executed.",
                ));
            }
            trace.push(turn.round);
            // Save the full plan before even delivering a request to the caller.
            if !checkpoint(&sender, &trace, &cancel).await? {
                return Ok(AgentOutcome {
                    outcome: StreamOutcome::Cancelled,
                    trace,
                });
            }
            for call in calls {
                let (reply, result) = oneshot::channel();
                if !emit(
                    &sender,
                    AgentEvent::ToolRequest {
                        call: call.clone(),
                        reply,
                    },
                    &cancel,
                )
                .await
                {
                    return Ok(AgentOutcome {
                        outcome: StreamOutcome::Cancelled,
                        trace,
                    });
                }
                let decision = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Ok(AgentOutcome { outcome: StreamOutcome::Cancelled, trace }),
                    result = tokio::time::timeout(Duration::from_secs(300), result) => match result {
                        Ok(Ok(result)) => result,
                        _ => return Err(CoreError::new("The tool request ended without a confirmed result. Review its status before trying again.")),
                    }
                };
                let (content, is_error) = match decision {
                    ToolDecision::Result(content) => (content, false),
                    ToolDecision::Denied => ("Tool execution was declined by the user. Do not repeat this action without new approval.".into(), true),
                    ToolDecision::Failed(error) => (error, true),
                };
                let (content, is_error) = if content.len() > MAX_TOOL_TEXT_BYTES {
                    ("The tool finished, but its result exceeded the client's size limit. Do not repeat the action automatically.".into(), true)
                } else {
                    (content, is_error)
                };
                trace
                    .last_mut()
                    .expect("current tool round")
                    .results
                    .push(ToolResult {
                        tool_call_id: call.id,
                        content,
                        is_error,
                    });
                // Each outcome is durable before another tool or provider call.
                if !checkpoint(&sender, &trace, &cancel).await? {
                    return Ok(AgentOutcome {
                        outcome: StreamOutcome::Cancelled,
                        trace,
                    });
                }
            }
        }
        unreachable!("round limit returns above")
    }
}

pub(crate) async fn emit(
    sender: &mpsc::Sender<AgentEvent>,
    event: AgentEvent,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        result = sender.send(event) => result.is_ok(),
    }
}

async fn checkpoint(
    sender: &mpsc::Sender<AgentEvent>,
    trace: &[ToolRound],
    cancel: &CancellationToken,
) -> Result<bool, CoreError> {
    let (ack, saved) = oneshot::channel();
    if !emit(
        sender,
        AgentEvent::Checkpoint {
            trace: trace.to_vec(),
            ack,
        },
        cancel,
    )
    .await
    {
        return Ok(false);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(false),
        saved = tokio::time::timeout(Duration::from_secs(30), saved) => match saved {
            Ok(Ok(true)) => Ok(true),
            _ => Err(CoreError::new("The tool conversation could not be saved. No further tools were run; fix storage before continuing.")),
        },
    }
}
