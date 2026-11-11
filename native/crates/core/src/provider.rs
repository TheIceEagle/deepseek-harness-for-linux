use crate::tools::{recover_trace, validate_trace, MAX_TOOL_CALLS, MAX_TOOL_TEXT_BYTES};
use crate::{
    AgentEvent, CoreError, Message, MessageStatus, Role, Settings, ToolCall, ToolDefinition,
    ToolRound,
};
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Delta(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamOutcome {
    Completed,
    Cancelled,
}

#[derive(Clone)]
pub struct Provider {
    client: reqwest::Client,
}

impl Provider {
    pub fn new() -> Result<Self, CoreError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(90))
            .user_agent("deepseek-harness-native/0.2")
            .build()
            .map_err(|_| {
                CoreError::new("The network client could not start. Restart the app and try again.")
            })?;
        Ok(Self { client })
    }

    pub async fn stream(
        &self,
        settings: &Settings,
        api_key: &str,
        messages: &[Message],
        sender: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<StreamOutcome, CoreError> {
        let (events, mut receiver) = mpsc::channel(64);
        let request_cancel = cancel.clone();
        let request = async move {
            self.stream_turn(
                settings,
                api_key,
                messages,
                &[],
                &[],
                &events,
                &request_cancel,
                MAX_RESPONSE_BYTES,
            )
            .await
        };
        let forward_cancel = cancel.clone();
        let forward = async move {
            loop {
                let next = tokio::select! {
                    biased;
                    () = forward_cancel.cancelled() => break,
                    event = receiver.recv() => event,
                };
                let Some(AgentEvent::Delta(text)) = next else {
                    break;
                };
                tokio::select! {
                    biased;
                    () = forward_cancel.cancelled() => break,
                    sent = sender.send(StreamEvent::Delta(text)) => {
                        if sent.is_err() { forward_cancel.cancel(); break; }
                    }
                }
            }
        };
        let (turn, ()) = tokio::join!(request, forward);
        let turn = turn?;
        Ok(if turn.cancelled || cancel.is_cancelled() {
            StreamOutcome::Cancelled
        } else {
            StreamOutcome::Completed
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stream_turn(
        &self,
        settings: &Settings,
        api_key: &str,
        messages: &[Message],
        trace: &[ToolRound],
        tools: &[ToolDefinition],
        sender: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
        visible_budget: usize,
    ) -> Result<ProviderTurn, CoreError> {
        let mut turn = ProviderTurn::default();
        if cancel.is_cancelled() {
            turn.cancelled = true;
            return Ok(turn);
        }
        settings.validate()?;
        let endpoint = completion_url(&settings.base_url)?;
        validate_deepseek_history(&endpoint, messages, !tools.is_empty())?;
        let mut wire_messages = Vec::with_capacity(messages.len() + 1);
        if !settings.system_prompt.trim().is_empty() {
            wire_messages.push(json!({ "role": "system", "content": settings.system_prompt }));
        }
        for message in messages {
            if message.role == Role::Assistant && !message.tool_trace.is_empty() {
                append_trace(
                    &mut wire_messages,
                    &message.tool_trace,
                    message.status == MessageStatus::Complete,
                )?;
            } else if !(message.content.is_empty()
                || message.role == Role::Assistant && message.status == MessageStatus::Error)
            {
                let wire = json!({ "role": message.role, "content": message.content });
                wire_messages.push(wire);
            }
        }
        append_trace(&mut wire_messages, trace, true)?;
        if !wire_messages
            .iter()
            .any(|message| message["role"] == "user")
        {
            return Err(CoreError::new(
                "Write a message before starting a response.",
            ));
        }
        let mut body =
            json!({"model": settings.model.trim(), "messages": wire_messages, "stream": true});
        if !tools.is_empty() {
            body["tools"] = tools.iter().map(|tool| json!({
                "type": "function", "function": {"name": tool.name, "description": tool.description, "parameters": tool.input_schema}
            })).collect();
        }
        let body = serde_json::to_vec(&body).map_err(|_| {
            CoreError::new("The conversation could not be prepared. Try a new conversation.")
        })?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(CoreError::new(
                "This conversation is too long for the current client. Start a new conversation.",
            ));
        }
        let mut request = self
            .client
            .post(endpoint)
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        if !api_key.trim().is_empty() {
            let mut authorization = HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))
                .map_err(|_| {
                    CoreError::new(
                        "The API key contains invalid characters. Enter it again in Settings.",
                    )
                })?;
            authorization.set_sensitive(true);
            request = request.header(AUTHORIZATION, authorization);
        }
        let mut response = tokio::select! {
            biased;
            () = cancel.cancelled() => {turn.cancelled = true; return Ok(turn);},
            response = request.send() => response.map_err(|_| connection_error())?,
        };
        if !response.status().is_success() {
            return Err(http_error(response.status().as_u16()));
        }
        if !response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        {
            return Err(CoreError::new("The provider did not return a streaming response. Check its API address and model."));
        }
        let mut parser = EventParser::default();
        let mut output_bytes = 0usize;
        let mut finish: Option<String> = None;
        let mut fragments: Vec<CallFragments> = Vec::new();
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => {turn.cancelled = true; return Ok(turn);},
                chunk = response.chunk() => chunk.map_err(|_| stream_error())?,
            };
            let Some(chunk) = next else {
                if finish.is_some() {
                    return finalize(turn, fragments, finish.as_deref(), tools);
                }
                return Err(stream_error());
            };
            for event in parser.feed(&chunk)? {
                if event.trim().is_empty() {
                    continue;
                }
                if event.trim() == "[DONE]" {
                    return finalize(turn, fragments, finish.as_deref(), tools);
                }
                let item: Value = serde_json::from_str(&event).map_err(|_| protocol_error())?;
                if item.get("error").is_some_and(|error| !error.is_null()) {
                    return Err(CoreError::new("The provider could not complete the response. Check the model and try again."));
                }
                let choices = item
                    .get("choices")
                    .and_then(Value::as_array)
                    .ok_or_else(protocol_error)?;
                let Some(choice) = choices.first() else {
                    continue;
                };
                let delta = choice.get("delta").ok_or_else(protocol_error)?;
                if !delta.is_object() {
                    return Err(protocol_error());
                }
                if finish.is_some() && delta.as_object().is_some_and(|fields| !fields.is_empty()) {
                    return Err(protocol_error());
                }
                for field in ["content", "reasoning_content"] {
                    if let Some(value) = delta.get(field).filter(|value| !value.is_null()) {
                        let text = value.as_str().ok_or_else(protocol_error)?;
                        count_output(&mut output_bytes, text.len())?;
                        if field == "content" {
                            if turn.round.content.len().saturating_add(text.len()) > visible_budget
                            {
                                return Err(CoreError::new("The response exceeded the native client's current size limit. The partial response has been kept."));
                            }
                            turn.round.content.push_str(text);
                            if !text.is_empty()
                                && !crate::agent::emit(
                                    sender,
                                    AgentEvent::Delta(text.to_owned()),
                                    cancel,
                                )
                                .await
                            {
                                turn.cancelled = true;
                                return Ok(turn);
                            }
                        } else {
                            turn.round.reasoning_content.push_str(text);
                        }
                    }
                }
                if let Some(calls) = delta.get("tool_calls").filter(|value| !value.is_null()) {
                    for call in calls.as_array().ok_or_else(protocol_error)? {
                        let index = call
                            .get("index")
                            .and_then(Value::as_u64)
                            .ok_or_else(protocol_error)?;
                        if index >= MAX_TOOL_CALLS as u64 {
                            return Err(tool_error());
                        }
                        let index = index as usize;
                        fragments
                            .resize_with(fragments.len().max(index + 1), CallFragments::default);
                        let target = &mut fragments[index];
                        if call
                            .get("type")
                            .is_some_and(|value| value.as_str() != Some("function"))
                        {
                            return Err(tool_error());
                        }
                        if let Some(id) = call.get("id") {
                            append_fragment(&mut target.id, id, 256, &mut output_bytes)?;
                        }
                        if let Some(function) = call.get("function") {
                            if !function.is_object() {
                                return Err(tool_error());
                            }
                            if let Some(name) = function.get("name") {
                                append_fragment(&mut target.name, name, 64, &mut output_bytes)?;
                            }
                            if let Some(arguments) = function.get("arguments") {
                                append_fragment(
                                    &mut target.arguments,
                                    arguments,
                                    MAX_TOOL_TEXT_BYTES,
                                    &mut output_bytes,
                                )?;
                            }
                        }
                    }
                }
                if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
                    let reason = reason.as_str().ok_or_else(protocol_error)?;
                    if finish.is_some() {
                        return Err(protocol_error());
                    }
                    match reason {
                        "stop" | "tool_calls" => finish = Some(reason.to_owned()),
                        "length" => return Err(CoreError::new("The response reached the provider's length limit. The partial response has been kept.")),
                        "content_filter" => return Err(CoreError::new("The provider stopped this response under its content policy.")),
                        _ => return Err(CoreError::new("The provider interrupted this response. The partial response has been kept.")),
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct ProviderTurn {
    pub(crate) round: ToolRound,
    pub(crate) cancelled: bool,
}

#[derive(Default)]
struct CallFragments {
    id: String,
    name: String,
    arguments: String,
}

fn count_output(count: &mut usize, bytes: usize) -> Result<(), CoreError> {
    *count = count.saturating_add(bytes);
    if *count > MAX_RESPONSE_BYTES {
        return Err(CoreError::new("The response exceeded the native client's current size limit. The partial response has been kept."));
    }
    Ok(())
}

fn append_fragment(
    output: &mut String,
    fragment: &Value,
    limit: usize,
    count: &mut usize,
) -> Result<(), CoreError> {
    let fragment = fragment.as_str().ok_or_else(tool_error)?;
    if output.len().saturating_add(fragment.len()) > limit {
        return Err(tool_error());
    }
    count_output(count, fragment.len())?;
    output.push_str(fragment);
    Ok(())
}

fn finalize(
    mut turn: ProviderTurn,
    fragments: Vec<CallFragments>,
    finish: Option<&str>,
    tools: &[ToolDefinition],
) -> Result<ProviderTurn, CoreError> {
    if !fragments.is_empty() {
        if finish != Some("tool_calls") {
            return Err(tool_error());
        }
        let mut ids = std::collections::HashSet::new();
        for fragment in fragments {
            if fragment.id.is_empty()
                || !ids.insert(fragment.id.clone())
                || !tools.iter().any(|tool| tool.name == fragment.name)
            {
                return Err(tool_error());
            }
            let arguments: Value =
                serde_json::from_str(&fragment.arguments).map_err(|_| tool_error())?;
            if !arguments.is_object() {
                return Err(tool_error());
            }
            turn.round.tool_calls.push(ToolCall {
                id: fragment.id,
                name: fragment.name,
                arguments,
            });
        }
    } else if finish == Some("tool_calls") {
        return Err(tool_error());
    } else if turn.round.content.is_empty() {
        return Err(CoreError::new(
            "The provider returned an empty response. Try again or choose another model.",
        ));
    }
    Ok(turn)
}

fn append_trace(
    wire: &mut Vec<Value>,
    trace: &[ToolRound],
    include_terminal: bool,
) -> Result<(), CoreError> {
    validate_trace(trace)?;
    let mut trace = trace.to_vec();
    recover_trace(&mut trace);
    for round in trace {
        // Retry continues from recorded tool outcomes, not an unfinished answer.
        if !include_terminal && round.tool_calls.is_empty() {
            continue;
        }
        let mut assistant = json!({"role": "assistant", "content": round.content});
        if !round.reasoning_content.is_empty() {
            assistant["reasoning_content"] = round.reasoning_content.into();
        }
        if !round.tool_calls.is_empty() {
            assistant["tool_calls"] = round.tool_calls.iter().map(|call| json!({
                "id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments.to_string()}
            })).collect();
        }
        wire.push(assistant);
        for call in round.tool_calls {
            let result = round
                .results
                .iter()
                .find(|result| result.tool_call_id == call.id)
                .expect("recovered result");
            let content = if result.is_error {
                json!({"error": result.content}).to_string()
            } else {
                result.content.clone()
            };
            wire.push(json!({"role": "tool", "tool_call_id": call.id, "content": content}));
        }
    }
    Ok(())
}

fn tool_error() -> CoreError {
    CoreError::new("The provider returned an invalid or unavailable tool request. No tool from that response was executed.")
}

fn validate_deepseek_history(
    endpoint: &Url,
    messages: &[Message],
    using_tools: bool,
) -> Result<(), CoreError> {
    if !using_tools || endpoint.host_str() != Some("api.deepseek.com") {
        return Ok(());
    }
    let missing = messages.iter().any(|message| {
        if message.role != Role::Assistant {
            return false;
        }
        if message.tool_trace.is_empty() {
            return !message.content.is_empty() && message.status != MessageStatus::Error;
        }
        message
            .tool_trace
            .iter()
            .filter(|round| {
                message.status == MessageStatus::Complete || !round.tool_calls.is_empty()
            })
            .any(|round| round.reasoning_content.is_empty())
    });
    if missing {
        return Err(CoreError::new("This older conversation lacks the reasoning required for DeepSeek tools. Start a new conversation or disconnect tools."));
    }
    Ok(())
}

pub(crate) fn completion_url(base_url: &str) -> Result<Url, CoreError> {
    if base_url.len() > 2048 {
        return Err(endpoint_error());
    }
    let mut url = Url::parse(base_url.trim()).map_err(|_| endpoint_error())?;
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|address| address.is_loopback())
        }
        Some(Host::Domain(host)) => host.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if !matches!(url.scheme(), "http" | "https")
        || (url.scheme() == "http" && !loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(endpoint_error());
    }
    let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

#[derive(Default)]
struct EventParser {
    line: Vec<u8>,
    data: Vec<String>,
    event_bytes: usize,
    ignore_lf: bool,
}

impl EventParser {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, CoreError> {
        let mut events = Vec::new();
        for &byte in bytes {
            if self.ignore_lf {
                self.ignore_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' | b'\n' => {
                    self.ignore_lf = byte == b'\r';
                    let line = std::str::from_utf8(&self.line).map_err(|_| protocol_error())?;
                    if line.is_empty() {
                        if !self.data.is_empty() {
                            events.push(self.data.join("\n"));
                            self.data.clear();
                        }
                        self.event_bytes = 0;
                    } else if let Some(data) = line.strip_prefix("data:") {
                        let data = data.strip_prefix(' ').unwrap_or(data);
                        self.event_bytes = self.event_bytes.saturating_add(data.len() + 1);
                        if self.event_bytes > MAX_FRAME_BYTES || self.data.len() >= 4096 {
                            return Err(protocol_error());
                        }
                        self.data.push(data.to_owned());
                    }
                    self.line.clear();
                }
                _ => {
                    if self.line.len() >= MAX_FRAME_BYTES {
                        return Err(protocol_error());
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }
}

fn endpoint_error() -> CoreError {
    CoreError::new("Enter an HTTPS API base address without credentials, a query, or a fragment. HTTP is supported only for a local provider.")
}

fn http_error(status: u16) -> CoreError {
    match status {
        401 | 403 => CoreError::new("The provider rejected the API key. Check the key and account access in Settings."),
        404 => CoreError::new("The API route or model was not found. Check the base address and model in Settings."),
        429 => CoreError::new("The provider's rate or account limit was reached. Wait before trying again and check your account."),
        300..=399 => CoreError::new("The provider redirected the request. Enter its final HTTPS API address in Settings."),
        500..=599 => CoreError::new("The provider is temporarily unavailable. Try again shortly."),
        _ => CoreError::new("The provider rejected the request. Check the model and settings, then try again."),
    }
}

fn connection_error() -> CoreError {
    CoreError::new(
        "The provider could not be reached. Check your connection and API address, then try again.",
    )
}

fn stream_error() -> CoreError {
    CoreError::new("The connection ended before the response finished. The partial response has been kept; try again when connected.")
}

fn protocol_error() -> CoreError {
    CoreError::new("The provider returned an invalid streaming response. Check that its API supports Chat Completions streaming.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_preserves_split_utf8_multiline_and_crlf_events() {
        let bytes = ": keepalive\r\ndata: {\"choices\":\r\ndata: [{\"delta\":{\"content\":\"héllo 🌊\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n".as_bytes();
        let mut parser = EventParser::default();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(parser.feed(&[*byte]).unwrap());
        }
        assert_eq!(events.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(&events[0]).unwrap()["choices"][0]["delta"]["content"],
            "héllo 🌊"
        );
        assert_eq!(events[1], "[DONE]");
    }

    #[test]
    fn rejects_unbounded_and_invalid_utf8_frames() {
        let mut parser = EventParser::default();
        assert!(parser.feed(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err());
        let mut parser = EventParser::default();
        assert!(parser.feed(&[0xff, b'\n']).is_err());
    }

    #[test]
    fn validates_provider_addresses_without_exposing_them() {
        assert_eq!(
            completion_url("https://api.deepseek.com/v1/")
                .unwrap()
                .as_str(),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert!(completion_url("http://127.0.0.1:1234/v1").is_ok());
        for value in [
            "http://example.com",
            "https://secret@example.com",
            "https://example.com?token=secret",
            "file:///etc/passwd",
        ] {
            let error = completion_url(value).unwrap_err().to_string();
            assert!(!error.contains("secret"));
            assert!(!error.contains(value));
        }
    }

    #[test]
    fn deepseek_tools_reject_legacy_reasoning_gaps_before_any_http_request() {
        let deepseek = completion_url("https://api.deepseek.com").unwrap();
        let other = completion_url("https://compatible.example.test").unwrap();
        let mut answer = Message {
            id: "legacy".into(),
            role: Role::Assistant,
            content: "Previous answer".into(),
            status: MessageStatus::Complete,
            created_at: 0,
            tool_trace: vec![],
        };
        let error = validate_deepseek_history(&deepseek, &[answer.clone()], true).unwrap_err();
        assert!(error
            .to_string()
            .contains("Start a new conversation or disconnect tools"));
        assert!(validate_deepseek_history(&deepseek, &[answer.clone()], false).is_ok());
        assert!(validate_deepseek_history(&other, &[answer.clone()], true).is_ok());
        answer.tool_trace.push(ToolRound {
            content: "Previous answer".into(),
            reasoning_content: "Actual retained reasoning".into(),
            ..ToolRound::default()
        });
        assert!(validate_deepseek_history(&deepseek, &[answer.clone()], true).is_ok());
        answer.tool_trace[0].reasoning_content.clear();
        assert!(validate_deepseek_history(&deepseek, &[answer], true).is_err());
    }

    #[test]
    fn empty_reply_placeholders_and_omitted_failed_plain_answers_do_not_block_tools() {
        let endpoint = completion_url("https://api.deepseek.com").unwrap();
        for (content, status) in [
            ("", MessageStatus::Streaming),
            ("failed partial", MessageStatus::Error),
        ] {
            let message = Message {
                id: "partial".into(),
                role: Role::Assistant,
                content: content.into(),
                status,
                created_at: 0,
                tool_trace: vec![],
            };
            assert!(validate_deepseek_history(&endpoint, &[message], true).is_ok());
        }
    }
}
