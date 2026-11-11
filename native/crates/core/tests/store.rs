use harness_native_core::{MessageStatus, Role, Settings, Store};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn conversations_and_non_secret_settings_survive_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let settings = Settings {
        base_url: "https://example.com/v1".into(),
        model: "local-model".into(),
        system_prompt: "Be concise.".into(),
    };
    store.save_settings(settings.clone()).unwrap();
    let session = store.create_session().unwrap();
    store
        .append_message(
            &session.id,
            Role::User,
            "Explain café and 🌊",
            MessageStatus::Complete,
        )
        .unwrap();
    store
        .append_message(
            &session.id,
            Role::Assistant,
            "A partial answer",
            MessageStatus::Streaming,
        )
        .unwrap();
    drop(store);

    let store = Store::open(directory.path()).unwrap();
    assert_eq!(store.settings(), &settings);
    let session = store.session(&session.id).unwrap();
    assert_eq!(session.title, "Explain café and 🌊");
    assert_eq!(session.messages[0].content, "Explain café and 🌊");
    assert_eq!(session.messages[1].content, "A partial answer");
    assert_eq!(session.messages[1].status, MessageStatus::Interrupted);
    let serialized = fs::read_to_string(directory.path().join("state.json")).unwrap();
    assert!(!serialized.contains("api_key"));
    assert!(!serialized.contains("streaming"));
    assert_eq!(
        fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(directory.path().join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn completed_and_cancelled_partial_answers_keep_their_status() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store
        .append_message(&session.id, Role::User, "Hello", MessageStatus::Complete)
        .unwrap();
    store
        .append_message(&session.id, Role::Assistant, "", MessageStatus::Streaming)
        .unwrap();
    store
        .replace_last_assistant(
            &session.id,
            "Saved partial response",
            MessageStatus::Cancelled,
        )
        .unwrap();
    drop(store);
    let store = Store::open(directory.path()).unwrap();
    assert_eq!(
        store.session(&session.id).unwrap().messages[1].status,
        MessageStatus::Cancelled
    );
}

#[test]
fn a_second_writer_is_rejected_without_changing_the_profile() {
    let directory = tempfile::tempdir().unwrap();
    let mut first = Store::open(directory.path()).unwrap();
    first.create_session().unwrap();
    let before = fs::read(directory.path().join("state.json")).unwrap();
    assert!(Store::open(directory.path()).is_err());
    assert_eq!(
        fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    drop(first);
    assert!(Store::open(directory.path()).is_ok());
}

#[test]
fn corrupt_or_unsupported_state_is_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    for original in [
        b"{not json".as_slice(),
        b"{\"version\":42,\"settings\":{},\"sessions\":[]}",
    ] {
        fs::write(&path, original).unwrap();
        assert!(Store::open(directory.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[test]
fn an_unwritable_destination_keeps_the_previous_in_memory_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let original = store.settings().clone();
    fs::create_dir(directory.path().join("state.json")).unwrap();
    let changed = Settings {
        model: "replacement".into(),
        ..original.clone()
    };
    assert!(store.save_settings(changed).is_err());
    assert_eq!(store.settings(), &original);
    assert_eq!(
        fs::read_dir(directory.path()).unwrap().count(),
        2,
        "Failed atomic writes leave no temporary profile file"
    );
}

#[test]
fn drafts_survive_switching_and_begin_turn_commits_both_messages_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let first = store.create_session().unwrap();
    store
        .save_draft(&first.id, "My unsent café draft 🌊")
        .unwrap();
    let second = store.create_session().unwrap();
    store.save_draft(&second.id, "A second draft").unwrap();
    drop(store);
    let mut store = Store::open(directory.path()).unwrap();
    assert_eq!(store.draft(&first.id), "My unsent café draft 🌊");
    assert_eq!(store.draft(&second.id), "A second draft");
    store
        .begin_turn(&first.id, "My unsent café draft 🌊")
        .unwrap();
    let session = store.session(&first.id).unwrap();
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, Role::User);
    assert_eq!(session.messages[0].status, MessageStatus::Complete);
    assert_eq!(session.messages[1].role, Role::Assistant);
    assert_eq!(session.messages[1].status, MessageStatus::Streaming);
    assert_eq!(session.draft, "");
    assert_eq!(store.draft(&second.id), "A second draft");
}

#[test]
fn a_failed_begin_turn_keeps_the_unsent_draft_and_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store.save_draft(&session.id, "Keep this draft").unwrap();
    fs::remove_file(directory.path().join("state.json")).unwrap();
    fs::create_dir(directory.path().join("state.json")).unwrap();
    assert!(store.begin_turn(&session.id, "Keep this draft").is_err());
    assert_eq!(store.draft(&session.id), "Keep this draft");
    assert!(store.session(&session.id).unwrap().messages.is_empty());
}

fn pending_tools() -> Vec<harness_native_core::ToolRound> {
    vec![harness_native_core::ToolRound {
        content: "Checking the browser".into(),
        reasoning_content: "Preserve thinking-mode tool context".into(),
        tool_calls: vec![harness_native_core::ToolCall {
            id: "call_pending".into(),
            name: "browser_read".into(),
            arguments: serde_json::json!({"tab":1}),
        }],
        results: vec![],
    }]
}

#[test]
fn tool_plans_upgrade_only_on_use_and_restart_marks_unknown_outcomes_without_replay() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store.begin_turn(&session.id, "Read this tab").unwrap();
    let path = directory.path().join("state.json");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap()["version"],
        1
    );
    store
        .replace_last_agent(
            &session.id,
            "Checking the browser",
            MessageStatus::Streaming,
            pending_tools(),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap()["version"],
        2
    );
    drop(store);
    let store = Store::open(directory.path()).unwrap();
    let message = &store.session(&session.id).unwrap().messages[1];
    assert_eq!(message.status, MessageStatus::Interrupted);
    assert_eq!(message.tool_trace[0].results.len(), 1);
    assert!(message.tool_trace[0].results[0].is_error);
    assert!(message.tool_trace[0].results[0]
        .content
        .contains("outcome is unknown"));
    assert!(message.tool_trace[0].results[0]
        .content
        .contains("explicit new approval"));
    let after = fs::read(&path).unwrap();
    drop(store);
    drop(Store::open(directory.path()).unwrap());
    assert_eq!(
        fs::read(&path).unwrap(),
        after,
        "Recovery is idempotent and never reissues a tool"
    );
}

#[test]
fn failed_tool_checkpoint_rolls_back_content_trace_and_profile_version() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store.begin_turn(&session.id, "Read this tab").unwrap();
    fs::remove_file(directory.path().join("state.json")).unwrap();
    fs::create_dir(directory.path().join("state.json")).unwrap();
    assert!(store
        .replace_last_agent(
            &session.id,
            "Never committed",
            MessageStatus::Streaming,
            pending_tools()
        )
        .is_err());
    let message = &store.session(&session.id).unwrap().messages[1];
    assert_eq!(message.content, "");
    assert!(message.tool_trace.is_empty());
    fs::remove_dir(directory.path().join("state.json")).unwrap();
    store.save_draft(&session.id, "Keep the draft").unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(saved["version"], 1);
}

#[test]
fn unconfirmed_tool_outcomes_cannot_be_saved_as_completed() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    let session = store.create_session().unwrap();
    store.begin_turn(&session.id, "Read this tab").unwrap();
    let before = fs::read(directory.path().join("state.json")).unwrap();
    assert!(store
        .replace_last_agent(
            &session.id,
            "Pretend completion",
            MessageStatus::Complete,
            pending_tools()
        )
        .is_err());
    assert_eq!(
        fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    assert!(store.session(&session.id).unwrap().messages[1]
        .tool_trace
        .is_empty());
}
