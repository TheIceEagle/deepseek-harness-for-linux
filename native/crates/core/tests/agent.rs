use harness_native_core::{
    AgentEvent, Message, MessageStatus, Provider, Role, Settings, Store, StreamOutcome,
    ToolDecision, ToolDefinition, ToolRound,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;

fn message() -> Message {
    Message {
        id: "user".into(),
        role: Role::User,
        content: "Use the echo tool.".into(),
        status: MessageStatus::Complete,
        created_at: 0,
        tool_trace: vec![],
    }
}

fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "fixture_echo".into(),
        description: "Echo text from an isolated test.".into(),
        input_schema: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
    }
}

async fn request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 64 * 1024);
    }
    let header = String::from_utf8(bytes).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length < 8 * 1024 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn event(delta: Value, finish: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":delta,"finish_reason":finish}]})
    )
}

fn tool(id: &str, name: &str, arguments: &str) -> String {
    format!(
        "{}{}data: [DONE]\n\n",
        event(
            json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":arguments}}]}),
            None
        ),
        event(json!({}), Some("tool_calls"))
    )
}

fn answer(text: &str) -> String {
    format!(
        "{}{}data: [DONE]\n\n",
        event(json!({"content":text}), None),
        event(json!({}), Some("stop"))
    )
}

async fn respond(socket: &mut TcpStream, body: &str) {
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

async fn fixture(responses: Vec<String>) -> (Settings, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let settings = Settings {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        ..Settings::default()
    };
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in responses {
            let (mut socket, _) = timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            requests.push(request(&mut socket).await);
            respond(&mut socket, &body).await;
        }
        requests
    });
    (settings, task)
}

#[tokio::test]
async fn fragmented_calls_preserve_reasoning_and_checkpoint_every_execution_before_continuing() {
    let first = format!(
        "{}{}{}data: [DONE]\n\n",
        event(
            json!({"content":"Checking.","reasoning_content":"Reasoning café 🌊", "tool_calls":[
            {"index":0,"id":"call_a","type":"function","function":{"name":"fixture_","arguments":"{\"text\":\"caf"}},
            {"index":1,"id":"call_b","function":{"name":"fixture_echo","arguments":"{\"text\":\"second\"}"}}]}),
            None
        ),
        event(
            json!({"tool_calls":[{"index":0,"function":{"name":"echo","arguments":"é 🌊\"}"}}]}),
            None
        ),
        event(json!({}), Some("tool_calls"))
    );
    let (settings, server) = fixture(vec![first, answer("Both echoes completed.")]).await;
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store.begin_turn(&session.id, "Use the echo tool.").unwrap();
    let messages = store.session(&session.id).unwrap().messages.clone();
    let (sender, mut events) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .run_agent(
                &settings,
                "synthetic-api-key-not-a-real-secret",
                &messages,
                &[definition()],
                sender,
                CancellationToken::new(),
            )
            .await
    });
    let mut visible = String::new();
    let mut calls = Vec::new();
    let mut checkpoints = Vec::new();
    while let Some(event) = timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
    {
        match event {
            AgentEvent::Delta(text) => visible.push_str(&text),
            AgentEvent::Checkpoint { trace, ack } => {
                assert!(
                    timeout(Duration::from_millis(25), events.recv())
                        .await
                        .is_err(),
                    "No next action or model request can proceed before durable acknowledgement"
                );
                checkpoints.push(trace.iter().map(|round| round.results.len()).sum::<usize>());
                store
                    .replace_last_agent(&session.id, &visible, MessageStatus::Streaming, trace)
                    .unwrap();
                ack.send(true).unwrap();
            }
            AgentEvent::ToolRequest { call, reply } => {
                let persisted = &store
                    .session(&session.id)
                    .unwrap()
                    .messages
                    .last()
                    .unwrap()
                    .tool_trace;
                assert!(persisted[0]
                    .tool_calls
                    .iter()
                    .any(|planned| planned.id == call.id));
                assert_eq!(
                    persisted[0].results.len(),
                    calls.len(),
                    "Every earlier execution is durable before the next starts"
                );
                calls.push(call.id.clone());
                reply
                    .send(ToolDecision::Result(
                        call.arguments["text"].as_str().unwrap().to_owned(),
                    ))
                    .unwrap();
            }
        }
    }
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(outcome.outcome, StreamOutcome::Completed);
    assert_eq!(calls, ["call_a", "call_b"]);
    assert_eq!(checkpoints, [0, 1, 2, 2]);
    assert_eq!(visible, "Checking.\n\nBoth echoes completed.");
    store
        .replace_last_agent(
            &session.id,
            &visible,
            MessageStatus::Complete,
            outcome.trace.clone(),
        )
        .unwrap();
    drop(store);
    let store = Store::open(directory.path()).unwrap();
    assert_eq!(
        store
            .session(&session.id)
            .unwrap()
            .messages
            .last()
            .unwrap()
            .tool_trace,
        outcome.trace
    );
    let saved = std::fs::read_to_string(directory.path().join("state.json")).unwrap();
    assert!(!saved.contains("synthetic-api-key"));
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap()["version"], 2);
    let requests = server.await.unwrap();
    assert_eq!(requests[0]["tools"][0]["function"]["name"], "fixture_echo");
    let wire = requests[1]["messages"].as_array().unwrap();
    assert_eq!(wire[1]["reasoning_content"], "Reasoning café 🌊");
    assert_eq!(
        wire[1]["tool_calls"][0]["function"]["arguments"],
        "{\"text\":\"café 🌊\"}"
    );
    assert_eq!(
        wire[2],
        json!({"role":"tool","tool_call_id":"call_a","content":"café 🌊"})
    );
    assert_eq!(wire[3]["tool_call_id"], "call_b");
}

#[tokio::test]
async fn a_failed_plan_save_never_dispatches_a_tool_or_another_model_request() {
    let (settings, server) = fixture(vec![tool("call_a", "fixture_echo", "{}")]).await;
    let (sender, mut events) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .run_agent(
                &settings,
                "",
                &[message()],
                &[definition()],
                sender,
                CancellationToken::new(),
            )
            .await
    });
    let AgentEvent::Checkpoint { trace, ack } = events.recv().await.unwrap() else {
        panic!("The tool plan must save first");
    };
    assert_eq!(trace[0].results.len(), 0);
    ack.send(false).unwrap();
    assert!(events.recv().await.is_none());
    assert!(task
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("could not be saved"));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn denial_is_persisted_as_a_tool_error_and_returned_to_the_model() {
    let (settings, server) = fixture(vec![
        tool("call_a", "fixture_echo", "{}"),
        answer("The action was declined."),
    ])
    .await;
    let (sender, mut events) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .run_agent(
                &settings,
                "",
                &[message()],
                &[definition()],
                sender,
                CancellationToken::new(),
            )
            .await
    });
    let mut requests = 0;
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::Checkpoint { ack, .. } => ack.send(true).unwrap(),
            AgentEvent::ToolRequest { reply, .. } => {
                requests += 1;
                reply.send(ToolDecision::Denied).unwrap();
            }
            AgentEvent::Delta(_) => {}
        }
    }
    let result = task.await.unwrap().unwrap();
    assert_eq!(requests, 1);
    assert!(result.trace[0].results[0].is_error);
    let captured = server.await.unwrap();
    assert!(captured[1]["messages"][2]["content"]
        .as_str()
        .unwrap()
        .contains("declined by the user"));
}

#[tokio::test]
async fn malformed_unknown_duplicate_or_unfinished_calls_never_reach_execution() {
    let cases = vec![
        tool("call_a", "unregistered", "{}"),
        tool("call_a", "fixture_echo", "{not json}"),
        tool("call_a", "fixture_echo", "[]"),
        format!(
            "{}data: [DONE]\n\n",
            event(
                json!({"tool_calls":[{"index":0,"id":"a","function":{"name":"fixture_echo","arguments":"{}"}}]}),
                None
            )
        ),
        format!(
            "{}{}data: [DONE]\n\n",
            event(
                json!({"tool_calls":[{"index":0,"id":"same","function":{"name":"fixture_echo","arguments":"{}"}},{"index":1,"id":"same","function":{"name":"fixture_echo","arguments":"{}"}}]}),
                None
            ),
            event(json!({}), Some("tool_calls"))
        ),
        tool(
            "call_a",
            "fixture_echo",
            &format!("{{\"x\":\"{}\"}}", "x".repeat(65536)),
        ),
    ];
    for response in cases {
        let (settings, server) = fixture(vec![response]).await;
        let (sender, mut events) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            Provider::new()
                .unwrap()
                .run_agent(
                    &settings,
                    "",
                    &[message()],
                    &[definition()],
                    sender,
                    CancellationToken::new(),
                )
                .await
        });
        while let Some(event) = events.recv().await {
            assert!(
                matches!(event, AgentEvent::Delta(_)),
                "No invalid plan can be checkpointed or executed"
            );
        }
        assert!(task.await.unwrap().is_err());
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn stop_while_awaiting_a_tool_result_preserves_an_uncertain_outcome_without_replay() {
    let (settings, server) = fixture(vec![tool("call_a", "fixture_echo", "{}")]).await;
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let (sender, mut events) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .run_agent(
                &settings,
                "",
                &[message()],
                &[definition()],
                sender,
                worker_cancel,
            )
            .await
    });
    let AgentEvent::Checkpoint { ack, .. } = events.recv().await.unwrap() else {
        panic!()
    };
    ack.send(true).unwrap();
    let AgentEvent::ToolRequest { reply, .. } = events.recv().await.unwrap() else {
        panic!()
    };
    cancel.cancel();
    let outcome = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.outcome, StreamOutcome::Cancelled);
    assert!(outcome.trace[0].results.is_empty());
    drop(reply);
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store.begin_turn(&session.id, "Please echo").unwrap();
    store
        .replace_last_agent(&session.id, "", MessageStatus::Cancelled, outcome.trace)
        .unwrap();
    drop(store);
    let store = Store::open(directory.path()).unwrap();
    let saved = &store.session(&session.id).unwrap().messages[1];
    assert_eq!(saved.status, MessageStatus::Cancelled);
    assert!(saved.tool_trace[0].results[0]
        .content
        .contains("outcome is unknown"));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_round_budget_prevents_a_ninth_tool_execution() {
    let responses = (0..9)
        .map(|index| tool(&format!("call_{index}"), "fixture_echo", "{}"))
        .collect();
    let (settings, server) = fixture(responses).await;
    let (sender, mut events) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .run_agent(
                &settings,
                "",
                &[message()],
                &[definition()],
                sender,
                CancellationToken::new(),
            )
            .await
    });
    let mut count = 0;
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::Checkpoint { ack, .. } => ack.send(true).unwrap(),
            AgentEvent::ToolRequest { reply, .. } => {
                count += 1;
                reply.send(ToolDecision::Result("done".into())).unwrap();
            }
            AgentEvent::Delta(_) => {}
        }
    }
    assert_eq!(count, 8);
    assert!(task
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("tool-round limit"));
    assert_eq!(server.await.unwrap().len(), 9);
}

#[tokio::test]
async fn a_failed_replys_saved_tool_history_is_included_without_reissuing_its_calls() {
    let (settings, server) = fixture(vec![answer("The earlier action remains recorded.")]).await;
    let mut failed = message();
    failed.role = Role::Assistant;
    failed.status = MessageStatus::Error;
    failed.content = "Visible combined text is not duplicated in the wire history".into();
    failed.tool_trace = vec![ToolRound {
        content: "Checking".into(),
        reasoning_content: "prior reasoning".into(),
        tool_calls: vec![harness_native_core::ToolCall {
            id: "prior_call".into(),
            name: "fixture_echo".into(),
            arguments: json!({}),
        }],
        results: vec![harness_native_core::ToolResult {
            tool_call_id: "prior_call".into(),
            content: "An action already happened".into(),
            is_error: false,
        }],
    }];
    let (sender, mut events) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .run_agent(
                &settings,
                "",
                &[message(), failed],
                &[definition()],
                sender,
                CancellationToken::new(),
            )
            .await
    });
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::Delta(_) => {}
            AgentEvent::Checkpoint { ack, .. } => ack.send(true).unwrap(),
            AgentEvent::ToolRequest { .. } => panic!("Earlier tools are never replayed"),
        }
    }
    assert_eq!(
        task.await.unwrap().unwrap().outcome,
        StreamOutcome::Completed
    );
    let captured = server.await.unwrap();
    assert_eq!(
        captured[0]["messages"][1]["tool_calls"][0]["id"],
        "prior_call"
    );
    assert_eq!(
        captured[0]["messages"][2]["content"],
        "An action already happened"
    );
    assert!(!captured[0].to_string().contains("Visible combined"));
}

#[tokio::test]
async fn reasoning_is_retained_when_tools_are_available_but_the_model_answers_directly() {
    let first = format!(
        "{}{}data: [DONE]\n\n",
        event(
            json!({"reasoning_content":"Actual direct-answer reasoning","content":"A direct answer"}),
            None
        ),
        event(json!({}), Some("stop"))
    );
    let (settings, server) = fixture(vec![first, answer("A follow-up answer")]).await;
    let mut history = vec![message()];
    for index in 0..2 {
        let settings = settings.clone();
        let snapshot = history.clone();
        let (sender, mut events) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            Provider::new()
                .unwrap()
                .run_agent(
                    &settings,
                    "",
                    &snapshot,
                    &[definition()],
                    sender,
                    CancellationToken::new(),
                )
                .await
        });
        let mut text = String::new();
        while let Some(event) = events.recv().await {
            match event {
                AgentEvent::Delta(delta) => text.push_str(&delta),
                AgentEvent::Checkpoint { ack, .. } => ack.send(true).unwrap(),
                AgentEvent::ToolRequest { .. } => panic!("The fixture never asks for a tool"),
            }
        }
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(
            outcome.trace.len(),
            1,
            "Tool-enabled direct answers retain canonical reasoning too"
        );
        if index == 0 {
            assert_eq!(
                outcome.trace[0].reasoning_content,
                "Actual direct-answer reasoning"
            );
            let mut assistant = message();
            assistant.role = Role::Assistant;
            assistant.content = text;
            assistant.tool_trace = outcome.trace;
            history.push(assistant);
            let mut follow_up = message();
            follow_up.id = "followup".into();
            follow_up.content = "Continue".into();
            history.push(follow_up);
        }
    }
    let captured = server.await.unwrap();
    assert_eq!(
        captured[1]["messages"][1]["reasoning_content"],
        "Actual direct-answer reasoning"
    );
}
