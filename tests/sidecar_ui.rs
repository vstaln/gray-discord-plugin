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
