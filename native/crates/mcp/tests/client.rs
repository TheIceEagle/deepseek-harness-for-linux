use harness_native_mcp::{McpClient, McpError, McpServerConfig};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn config(mode: &str, marker: Option<&Path>) -> McpServerConfig {
    let mut args = vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixture.py")
            .to_string_lossy()
            .into_owned(),
        mode.into(),
    ];
    if let Some(marker) = marker {
        args.push(marker.to_string_lossy().into_owned());
    }
    McpServerConfig {
        id: "fixture".into(),
        command: "/usr/bin/python3".into(),
        args,
        timeout_secs: 3,
        ..Default::default()
    }
}

async fn connected(mode: &str) -> McpClient {
    McpClient::connect(&config(mode, None), CancellationToken::new())
        .await
        .unwrap()
}

fn tool(client: &McpClient) -> String {
    client
        .tools()
        .iter()
        .find(|tool| tool.original_name == "echo")
        .unwrap()
        .name
        .clone()
}

fn pid(path: &Path) -> u32 {
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

fn running(pid: u32) -> bool {
    // An orphaned grandchild can briefly await the host init reaper after its
    // parent dies. A zombie has stopped executing and cannot hold pipe handles.
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.split(')')
            .nth(1)
            .is_some_and(|tail| !tail.trim_start().starts_with('Z'))
    })
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_stdio_handshake_pagination_and_unicode_tool_results() {
    let client = connected("echo").await;
    assert_eq!(client.tools().len(), 2);
    assert!(client
        .tools()
        .iter()
        .all(|tool| tool.name.starts_with("mcp__fixture__") && tool.name.len() <= 64));
    let answer = client
        .call(
            &tool(&client),
            json!({"text": "Rust café 世界 🌊"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(answer, "Rust café 世界 🌊");
    client.close().await;
    assert!(!client.is_connected());
    assert_eq!(
        client
            .call(&tool(&client), json!({}), CancellationToken::new())
            .await,
        Err(McpError::Disconnected)
    );
}

#[tokio::test]
async fn rejects_unknown_tools_and_invalid_arguments_before_execution() {
    let client = connected("echo").await;
    assert_eq!(
        client
            .call("not-discovered", json!({}), CancellationToken::new())
            .await,
        Err(McpError::UnknownTool)
    );
    assert_eq!(
        client
            .call(&tool(&client), json!([]), CancellationToken::new())
            .await,
        Err(McpError::InvalidArguments)
    );
    assert_eq!(
        client
            .call(
                &tool(&client),
                json!({"text": "x".repeat(65536)}),
                CancellationToken::new()
            )
            .await,
        Err(McpError::InvalidArguments)
    );
    client.close().await;
}

#[tokio::test]
async fn a_missing_working_folder_is_valid_saved_configuration_but_cannot_connect() {
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let mut server = config("echo", Some(&marker));
    server.cwd = Some(scratch.path().join("missing-workspace"));
    assert!(server.validate().is_ok());
    assert!(matches!(
        McpClient::connect(&server, CancellationToken::new()).await,
        Err(McpError::InvalidConfiguration)
    ));
    assert!(!marker.exists());
}

#[tokio::test]
async fn failures_are_fixed_and_results_are_bounded_or_explicitly_unsupported() {
    for (mode, expected) in [
        ("error", McpError::ToolFailed),
        ("tool_error", McpError::ToolFailed),
        ("image", McpError::UnsupportedContent),
        ("large_output", McpError::OutputTooLarge),
    ] {
        let client = connected(mode).await;
        let result = client
            .call(&tool(&client), json!({}), CancellationToken::new())
            .await;
        assert_eq!(result, Err(expected));
        assert!(!result
            .unwrap_err()
            .to_string()
            .contains("synthetic-private-error-marker"));
        client.close().await;
    }
    let client = connected("structured").await;
    let result: Value = serde_json::from_str(
        &client
            .call(&tool(&client), json!({}), CancellationToken::new())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result, json!({"value": "café 🌊"}));
    client.close().await;
}

#[tokio::test]
async fn rejects_duplicate_oversized_and_cyclic_catalogs_without_leaking_processes() {
    for (mode, expected) in [
        ("duplicate", McpError::InvalidTools),
        ("many", McpError::TooManyTools),
        ("cycle", McpError::InvalidTools),
        ("large_schema", McpError::InvalidTools),
    ] {
        let scratch = TempDir::new().unwrap();
        let marker = scratch.path().join("pid");
        let result =
            McpClient::connect(&config(mode, Some(&marker)), CancellationToken::new()).await;
        assert!(matches!(result, Err(error) if error == expected));
        let child = pid(&marker);
        wait_until(|| !PathBuf::from(format!("/proc/{child}")).exists()).await;
    }
}

#[tokio::test]
async fn cancelled_operations_stop_uncooperative_server_and_reap_it() {
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let client = McpClient::connect(&config("hang", Some(&marker)), CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let name = tool(&client);
    let copy = client.clone();
    let task = tokio::spawn(async move { copy.call(&name, json!({}), token).await });
    wait_until(|| marker.with_extension("called").exists()).await;
    cancel.cancel();
    assert_eq!(task.await.unwrap(), Err(McpError::Cancelled));
    assert!(!client.is_connected());
    assert!(!PathBuf::from(format!("/proc/{}", pid(&marker))).exists());
}

#[tokio::test]
async fn a_tool_timeout_disconnects_and_reaps_the_server() {
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let mut server = config("hang", Some(&marker));
    server.timeout_secs = 1;
    let client = McpClient::connect(&server, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        client
            .call(&tool(&client), json!({}), CancellationToken::new())
            .await,
        Err(McpError::Timeout)
    );
    assert!(!client.is_connected());
    assert!(!PathBuf::from(format!("/proc/{}", pid(&marker))).exists());
}

#[tokio::test]
async fn timeouts_and_oversized_wire_lines_stop_and_reap_startup_servers() {
    for mode in ["stall", "huge_line"] {
        let scratch = TempDir::new().unwrap();
        let marker = scratch.path().join("pid");
        let mut server = config(mode, Some(&marker));
        server.timeout_secs = 1;
        assert!(matches!(
            McpClient::connect(&server, CancellationToken::new()).await,
            Err(McpError::Timeout)
        ));
        let child = pid(&marker);
        wait_until(|| !PathBuf::from(format!("/proc/{child}")).exists()).await;
    }
}

#[tokio::test]
async fn close_kills_grandchildren_even_after_server_parent_exits() {
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let client = McpClient::connect(
        &config("grandchild", Some(&marker)),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    client
        .call(&tool(&client), json!({}), CancellationToken::new())
        .await
        .unwrap();
    let descendant = pid(&marker.with_extension("grandchild"));
    client.close().await;
    wait_until(|| !running(descendant)).await;
    assert!(!PathBuf::from(format!("/proc/{}", pid(&marker))).exists());
}

#[tokio::test]
async fn final_clone_drop_stops_and_reaps_the_server() {
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let client = McpClient::connect(&config("echo", Some(&marker)), CancellationToken::new())
        .await
        .unwrap();
    let copy = client.clone();
    drop(client);
    assert!(running(pid(&marker)));
    drop(copy);
    wait_until(|| !PathBuf::from(format!("/proc/{}", pid(&marker))).exists()).await;
}

#[tokio::test]
async fn concurrent_clone_closes_both_wait_for_the_server_to_be_reaped() {
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let client = McpClient::connect(&config("echo", Some(&marker)), CancellationToken::new())
        .await
        .unwrap();
    let copy = client.clone();
    let process = pid(&marker);
    let first = tokio::spawn(async move {
        copy.close().await;
        assert!(!PathBuf::from(format!("/proc/{process}")).exists());
    });
    client.close().await;
    assert!(!PathBuf::from(format!("/proc/{process}")).exists());
    first.await.unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn an_abrupt_parent_death_stops_the_direct_mcp_server() {
    if let Some(marker) = std::env::var_os("HARNESS_MCP_PARENT_DEATH_CHILD") {
        let marker = PathBuf::from(marker);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let _client =
                McpClient::connect(&config("echo", Some(&marker)), CancellationToken::new())
                    .await
                    .unwrap();
            std::fs::write(marker.with_extension("ready"), "ready").unwrap();
            // The outer test kills this helper process while its Rust client
            // remains alive, simulating SIGKILL of the application itself.
            std::thread::sleep(Duration::from_secs(60));
        });
        return;
    }
    let scratch = TempDir::new().unwrap();
    let marker = scratch.path().join("pid");
    let mut helper = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_abrupt_parent_death_stops_the_direct_mcp_server",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HARNESS_MCP_PARENT_DEATH_CHILD", &marker)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !marker.with_extension("ready").exists() {
        if started.elapsed() > Duration::from_secs(5) {
            let _ = helper.kill();
            let _ = helper.wait();
            panic!("The isolated parent-death fixture did not start.");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let server = pid(&marker);
    assert!(running(server));
    helper.kill().unwrap();
    helper.wait().unwrap();
    let started = std::time::Instant::now();
    while running(server) && started.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !running(server),
        "The direct MCP server outlived its killed parent."
    );
}

#[test]
fn explicit_environment_is_isolated_from_model_credentials_and_ambient_secrets() {
    // The test child receives synthetic keys only. Never read or forward the
    // developer's actual environment secrets into a fixture or test report.
    if std::env::var_os("HARNESS_MCP_TEST_CHILD").is_some() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut server = config("environment", None);
            server.env_names = vec!["HARNESS_MCP_EXPLICIT".into()];
            let client = McpClient::connect(&server, CancellationToken::new())
                .await
                .unwrap();
            let result = client
                .call(&tool(&client), json!({}), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&result).unwrap(),
                json!({"modelKey": false, "ambientSecret": false, "explicit": true})
            );
            assert!(!serde_json::to_string(&server)
                .unwrap()
                .contains("per-server-marker"));
            client.close().await;
        });
        return;
    }
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "explicit_environment_is_isolated_from_model_credentials_and_ambient_secrets",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HARNESS_MCP_TEST_CHILD", "1")
        .env("DEEPSEEK_API_KEY", "synthetic-model-credential-marker")
        .env("AWS_SECRET_ACCESS_KEY", "synthetic-ambient-secret-marker")
        .env("HARNESS_MCP_EXPLICIT", "per-server-marker")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "The isolated environment fixture failed."
    );
    let output = String::from_utf8_lossy(&result.stdout);
    let errors = String::from_utf8_lossy(&result.stderr);
    for marker in [
        "synthetic-model-credential-marker",
        "synthetic-ambient-secret-marker",
        "per-server-marker",
    ] {
        assert!(!output.contains(marker) && !errors.contains(marker));
    }
}
