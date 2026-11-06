use harness_native_core::{
    Message, MessageStatus, Provider, Role, Settings, StreamEvent, StreamOutcome,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

fn message() -> Message {
    Message {
        id: "test-message".into(),
        role: Role::User,
        content: "Hello, 🌊".into(),
        status: MessageStatus::Complete,
        created_at: 0,
    }
}

async fn request(socket: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let end = loop {
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 1);
        request.push(byte[0]);
        assert!(request.len() < 64 * 1024);
        if request.ends_with(b"\r\n\r\n") {
            break request.len();
        }
    };
    let header = String::from_utf8(request[..end].to_vec()).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|number| number.trim().parse::<usize>().unwrap())
        })
        .unwrap_or_default();
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    format!("{header}{}", String::from_utf8(body).unwrap())
}

fn event(content: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{"content":content},"finish_reason":null}]})
    )
}

const FINISH: &str =
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\r\n\r\n";

#[tokio::test]
async fn streams_before_completion_and_preserves_utf8_without_persisting_a_key() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (release, wait) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n").await.unwrap();
        socket
            .write_all(event("First fragment").as_bytes())
            .await
            .unwrap();
        wait.await.unwrap();
        for &byte in format!("{}{}", event(" café 🌊"), FINISH).as_bytes() {
            socket.write_all(&[byte]).await.unwrap();
        }
        request
    });
    let settings = Settings {
        base_url,
        model: "fixture".into(),
        ..Settings::default()
    };
    let (sender, mut receiver) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .stream(
                &settings,
                "test-secret-not-saved",
                &[message()],
                sender,
                CancellationToken::new(),
            )
            .await
    });
    let first = timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, StreamEvent::Delta("First fragment".into()));
    assert!(
        !task.is_finished(),
        "The provider is still gated when the first fragment reaches the app"
    );
    release.send(()).unwrap();
    assert_eq!(
        receiver.recv().await.unwrap(),
        StreamEvent::Delta(" café 🌊".into())
    );
    assert_eq!(task.await.unwrap().unwrap(), StreamOutcome::Completed);
    let captured = server.await.unwrap();
    assert!(captured.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(captured
        .to_ascii_lowercase()
        .contains("authorization: bearer test-secret-not-saved"));
    let body: Value = serde_json::from_str(captured.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], "fixture");
    assert_eq!(body["messages"][0]["content"], "Hello, 🌊");
    assert!(body.get("api_key").is_none());
}

#[tokio::test]
async fn cancellation_disconnects_an_active_provider_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let settings = Settings {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        ..Settings::default()
    };
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket.write_all(event("partial").as_bytes()).await.unwrap();
        let mut bytes = [0; 16];
        assert_eq!(
            timeout(Duration::from_secs(3), socket.read(&mut bytes))
                .await
                .unwrap()
                .unwrap(),
            0,
            "Cancellation drops the provider connection"
        );
    });
    let (sender, mut receiver) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let provider_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .stream(&settings, "", &[message()], sender, provider_cancel)
            .await
    });
    assert_eq!(
        receiver.recv().await.unwrap(),
        StreamEvent::Delta("partial".into())
    );
    cancel.cancel();
    assert_eq!(
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        StreamOutcome::Cancelled
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_remains_responsive_when_the_ui_queue_is_full() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let settings = Settings {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        ..Settings::default()
    };
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        socket
            .write_all(format!("{}{}", event("first"), event("second")).as_bytes())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let (sender, receiver) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let provider_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        Provider::new()
            .unwrap()
            .stream(&settings, "", &[message()], sender, provider_cancel)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    assert_eq!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        StreamOutcome::Cancelled
    );
    drop(receiver);
    server.abort();
}

#[tokio::test]
async fn errors_and_redirects_never_echo_credentials_or_provider_bodies() {
    for status in [302, 401, 429, 500] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let settings = Settings {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            ..Settings::default()
        };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: 26\r\nLocation: http://127.0.0.1:1/private-key\r\nConnection: close\r\n\r\nprivate-provider-body-secret").as_bytes()).await.unwrap();
        });
        let (sender, _) = mpsc::channel(1);
        let error = Provider::new()
            .unwrap()
            .stream(
                &settings,
                "private-key",
                &[message()],
                sender,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-key"));
        assert!(!error.to_string().contains("provider-body-secret"));
        assert!(!format!("{error:?}").contains("private-key"));
        if status == 302 {
            assert!(
                error.to_string().contains("redirected"),
                "The client rejects a redirect without contacting its target"
            );
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn truncated_or_malformed_streams_fail_instead_of_reporting_success() {
    for response in [event("partial"), "data: {not json}\n\n".into()] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let settings = Settings {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            ..Settings::default()
        };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).as_bytes()).await.unwrap();
        });
        let (sender, _receiver) = mpsc::channel(8);
        assert!(Provider::new()
            .unwrap()
            .stream(
                &settings,
                "",
                &[message()],
                sender,
                CancellationToken::new()
            )
            .await
            .is_err());
        server.await.unwrap();
    }
}
