//! Exercise the real Rust MCP client against the optional pinned Cordis host.
use harness_native_mcp::{McpClient, McpServerConfig};
use serde_json::json;
use std::{error::Error, path::PathBuf};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let runtime = PathBuf::from(
        arguments
            .next()
            .ok_or("Supply the pinned runtime directory.")?,
    )
    .canonicalize()?;
    if arguments.next().is_some() {
        return Err("Supply only the pinned runtime directory.".into());
    }
    let scratch = tempfile::tempdir()?;
    let workspace = scratch.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let profile = scratch.path().join("js-compat-profile");
    let host = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../compat/host.mjs")
        .canonicalize()?;
    let bundled_node = runtime.join("node");
    let node = if bundled_node.is_file() {
        bundled_node.to_string_lossy().into_owned()
    } else {
        "node".to_owned()
    };
    let config = McpServerConfig {
        id: "compat".to_owned(),
        command: node,
        args: vec![
            host.to_string_lossy().into_owned(),
            "--runtime".to_owned(),
            runtime.to_string_lossy().into_owned(),
            "--profile".to_owned(),
            profile.to_string_lossy().into_owned(),
            "--plugin".to_owned(),
            "@deepseek-ai/dsh-tool-working-directory".to_owned(),
        ],
        cwd: Some(workspace.clone()),
        env_names: Vec::new(),
        timeout_secs: 60,
    };
    let client = McpClient::connect(&config, CancellationToken::new()).await?;
    assert!(client.is_connected(), "The real host must be connected.");
    assert_eq!(
        client.tools().len(),
        1,
        "Default Harness tools stay hidden."
    );
    let tool = &client.tools()[0];
    assert_eq!(tool.original_name, "working_directory");
    let result = client
        .call(&tool.name, json!({}), CancellationToken::new())
        .await?;
    assert_eq!(result, workspace.to_string_lossy());
    assert!(profile.join("native-compat.json").is_file());
    client.close().await;
    assert!(
        !client.is_connected(),
        "Closing disconnects the shared client."
    );
    println!("Rust MCP and unchanged Harness JavaScript tool interoperability passed.");
    Ok(())
}
