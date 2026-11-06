use crate::{CoreError, Message, MessageStatus, Role, Settings};
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
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
        if cancel.is_cancelled() {
            return Ok(StreamOutcome::Cancelled);
        }
        settings.validate()?;
        let endpoint = completion_url(&settings.base_url)?;
        let mut wire_messages = Vec::with_capacity(messages.len() + 1);
        if !settings.system_prompt.trim().is_empty() {
            wire_messages.push(json!({ "role": "system", "content": settings.system_prompt }));
        }
        for message in messages {
            if message.content.is_empty()
                || (message.role == Role::Assistant && message.status == MessageStatus::Error)
            {
                continue;
            }
            wire_messages.push(json!({ "role": message.role, "content": message.content }));
        }
        if !wire_messages
            .iter()
            .any(|message| message["role"] == "user")
        {
            return Err(CoreError::new(
                "Write a message before starting a response.",
            ));
        }
        let body = serde_json::to_vec(&json!({
            "model": settings.model.trim(),
            "messages": wire_messages,
            "stream": true,
        }))
        .map_err(|_| {
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
            () = cancel.cancelled() => return Ok(StreamOutcome::Cancelled),
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
        let mut saw_finish = false;
        let mut saw_content = false;
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(StreamOutcome::Cancelled),
                chunk = response.chunk() => chunk.map_err(|_| stream_error())?,
            };
            let Some(chunk) = next else {
                // A finished choice is sufficient for compatible servers which
                // close the stream without the optional [DONE] sentinel.
                if saw_finish && saw_content {
                    return Ok(StreamOutcome::Completed);
                }
                return Err(stream_error());
            };
            for event in parser.feed(&chunk)? {
                if event.trim().is_empty() {
                    continue;
                }
                if event.trim() == "[DONE]" {
                    if saw_content {
                        return Ok(StreamOutcome::Completed);
                    }
                    return Err(CoreError::new("The provider returned an empty response. Try again or choose another model."));
                }
                let item: Value = serde_json::from_str(&event).map_err(|_| protocol_error())?;
                if item.get("error").is_some_and(|error| !error.is_null()) {
                    return Err(CoreError::new("The provider could not complete the response. Check the model and try again."));
                }
                let choices = item
                    .get("choices")
                    .and_then(Value::as_array)
                    .ok_or_else(protocol_error)?;
                // Usage-only chunks have an empty choices array.
                let Some(choice) = choices.first() else {
                    continue;
                };
                if let Some(content) = choice
                    .get("delta")
                    .and_then(|delta| delta.get("content"))
                    .and_then(Value::as_str)
                {
                    if !content.is_empty() {
                        output_bytes = output_bytes.saturating_add(content.len());
                        if output_bytes > MAX_RESPONSE_BYTES {
                            return Err(CoreError::new("The response exceeded the native client's current size limit. The partial response has been kept."));
                        }
                        saw_content = true;
                        tokio::select! {
                            biased;
                            () = cancel.cancelled() => return Ok(StreamOutcome::Cancelled),
                            sent = sender.send(StreamEvent::Delta(content.to_owned())) => {
                                if sent.is_err() { return Ok(StreamOutcome::Cancelled); }
                            }
                        }
                    }
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    match reason {
                        "stop" => saw_finish = true,
                        "length" => return Err(CoreError::new("The response reached the provider's length limit. The partial response has been kept.")),
                        "content_filter" => return Err(CoreError::new("The provider stopped this response under its content policy.")),
                        "tool_calls" => return Err(CoreError::new("This response requested a model tool call. Automatic agent tool calls are not available in this native preview.")),
                        _ => return Err(CoreError::new("The provider interrupted this response. The partial response has been kept.")),
                    }
                }
            }
        }
    }
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
}
