use gray_discord::sidecar;
use serde_json::json;

#[tokio::test]
async fn manifest_exposes_typed_ui_and_private_file_tools() {
    let manifest = sidecar::dispatch(
        "plugin/manifest",
        &json!({}),
        std::path::Path::new("config.json"),
    )
    .await;
    let tools = manifest["tools"].as_array().expect("tool list");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"discord_send_ui"));
    assert!(names.contains(&"discord_open_modal"));
    assert!(names.contains(&"discord_file"));
    assert!(names.contains(&"discord_ui_schema"));
    let send = tools
        .iter()
        .find(|tool| tool["name"] == "discord_send_ui")
        .unwrap();
    assert_eq!(
        send["parameters"]["properties"]["document"]["type"],
        "object"
    );
}

#[tokio::test]
async fn ui_schema_is_protocol_typed_and_bounded() {
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_ui_schema", "args": {"surface": "modal"}}),
        std::path::Path::new("config.json"),
    )
    .await;
    assert_eq!(result["protocol"], "gray.discord.ui");
    assert_eq!(result["limits"]["max_components"], 40);
    assert_eq!(result["document"]["components"][0]["type"], "label");
}

#[tokio::test]
async fn unknown_tool_returns_a_redacted_error() {
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_send", "args": {"content": "secret"}}),
        std::path::Path::new("missing-config.json"),
    )
    .await;
    assert_eq!(result["is_error"], true);
    assert!(!result.to_string().contains("secret"));
}

/// Regression: every tool reply must be readable from `content`. gray hands the
/// model only that field, so a reply without it is a dead tool — the schema
/// tool shipped exactly that bug ("reply missing content").
#[tokio::test]
async fn schema_reply_is_readable_from_content() {
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_ui_schema", "args": {"surface": "message"}}),
        std::path::Path::new("config.json"),
    )
    .await;
    let content = result["content"]
        .as_str()
        .expect("schema must be readable from content");
    assert!(content.contains("gray.discord.ui"), "content: {content}");
    assert!(content.contains("max_components"), "content: {content}");
}

/// Regression: `discord_file` import used to say only "Imported a private
/// managed Discord file." — the managed id stayed in a field gray never shows,
/// so no document could reference it and the import was a dead end.
#[tokio::test]
async fn imported_file_id_reaches_the_model() {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).expect("workdir");
    let config = dir.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::json!({
            "token": "test-token",
            "channel_id": "1544925612823547987",
            "owner_id": "1509856035576483874",
            "gray_bin": dir.path().join("gray").display().to_string(),
            "gray_home": dir.path().join("home").display().to_string(),
            "workdir": dir.path().display().to_string(),
        })
        .to_string(),
    )
    .expect("config");
    std::fs::write(work.join("slide.png"), b"\x89PNG\r\n\x1a\n").expect("png");

    // the conversation work directory is the process cwd, and tests in one
    // binary share it — this file is the only test here that touches it.
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&work).expect("chdir");
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_file", "args": {"action": "import", "path": "slide.png"}}),
        &config,
    )
    .await;
    std::env::set_current_dir(previous).expect("restore cwd");

    let content = result["content"]
        .as_str()
        .expect("import must report content");
    assert!(content.contains("file_id="), "content: {content}");
    assert_eq!(result["file"]["name"], "slide.png");
}

fn ui_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.json");
    std::fs::write(
        &config,
        json!({
            "token": "test-token",
            "channel_id": "1544925612823547987",
            "owner_id": "1509856035576483874",
        })
        .to_string(),
    )
    .expect("config");
    config
}

/// Regression: a malformed document said "Discord delivery failed. Run
/// gray-discord doctor", so the model blamed the transport and gave up. The
/// schema had listed numeric types (13), and this is the document it sent.
#[tokio::test]
async fn a_bad_document_says_what_is_wrong_and_that_nothing_was_sent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_send_ui", "args": {"document": {
            "version": 1, "document_id": "plunger-1",
            "components": [{"type": 13, "file_id": "838db4eb30929a6e2d407ed03f82332f"}]
        }}}),
        &ui_config(dir.path()),
    )
    .await;
    assert_eq!(result["is_error"], true);
    let content = result["content"].as_str().expect("content");
    assert!(
        content.starts_with("Not sent: invalid document"),
        "{content}"
    );
    assert!(!content.contains("doctor"), "{content}");
    assert!(!content.contains("test-token"), "{content}");
}

/// The schema must teach the names the parser accepts, never Discord's codes.
#[tokio::test]
async fn message_schema_lists_only_named_types() {
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_ui_schema", "args": {"surface": "message"}}),
        std::path::Path::new("config.json"),
    )
    .await;
    let components = result["document"]["components"]
        .as_array()
        .expect("components");
    assert!(
        components.iter().all(|c| c["type"].is_string()),
        "{components:?}"
    );
    assert_eq!(result["document"]["surface"], "message");
    // The example is a real, parseable document.
    let example = result["example"].clone();
    assert!(
        serde_json::from_value::<gray_discord::component_protocol::UiDocument>(example).is_ok()
    );
}
