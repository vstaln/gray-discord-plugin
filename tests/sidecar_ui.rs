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

fn compile_agent_document(raw: &serde_json::Value) -> Result<Vec<serde_json::Value>, String> {
    use gray_discord::component_compile::{
        compile_message, CompileContext, DeterministicAllocator,
    };
    use gray_discord::component_normalize::{coerce_document, parse_document};
    use gray_discord::component_protocol::{FileRef, Origin, Surface, UiDocument};
    let value = coerce_document(raw, Surface::Message)?;
    let UiDocument::Message(document) = parse_document(&value)? else {
        return Err("not a message".into());
    };
    let mut allocator = DeterministicAllocator::default().with_file(FileRef {
        id: "FILE_ID_FROM_discord_file".into(),
        name: "render.png".into(),
        media_type: "image/png".into(),
        size: 3,
        sha256: "00".into(),
    });
    let compiled = compile_message(
        &document,
        &mut CompileContext::new(Origin::Agent, &mut allocator),
    )
    .map_err(|error| error.to_string())?;
    gray_discord::render::validate_components(&compiled.components)?;
    Ok(compiled.components)
}

#[tokio::test]
async fn every_schema_example_compiles_to_valid_discord_wire_json() {
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_ui_schema", "args": {"surface": "message"}}),
        std::path::Path::new("config.json"),
    )
    .await;
    assert!(result["content"]
        .as_str()
        .is_some_and(|c| c.contains("examples")));
    let examples = result["examples"].as_object().expect("examples");
    assert!(examples.len() >= 4);
    for (name, example) in examples {
        compile_agent_document(example).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn raw_discord_style_documents_compile() {
    // What models typically write after reading Discord's own docs.
    let wire = compile_agent_document(&json!({
        "flags": 32768,
        "components": [{
            "type": 17,
            "accent_color": 703487,
            "components": [
                {"type": 10, "content": "# Real Game v7.3"},
                {"type": 14, "divider": true, "spacing": 2},
                {"type": 9, "components": [{"type": 10, "content": "Update notes"}],
                 "accessory": {"type": 11, "media": {"url": "https://example.com/preview.webp"}}},
                {"type": 12, "items": [{"media": {"url": "https://example.com/a.png"}, "description": "a"}]},
                {"type": 1, "components": [{"type": 2, "style": 5, "label": "Docs", "url": "https://example.com"}]}
            ]
        }]
    }))
    .unwrap();
    assert_eq!(wire[0]["type"], 17);
    assert_eq!(wire[0]["components"][1]["spacing"], 2);
}

#[test]
fn image_shortcuts_compile() {
    let wire = compile_agent_document(&json!({
        "title": "Look at this",
        "accent_color": "#EB459E",
        "components": [{"type": "media_gallery", "items": ["FILE_ID_FROM_discord_file"]}]
    }))
    .unwrap();
    let media = &wire[0]["components"][1]["items"][0]["media"]["url"];
    assert_eq!(media, "attachment://render.png");
}

#[tokio::test]
async fn invalid_documents_name_the_path_and_never_touch_discord() {
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_send_ui", "args": {"document": {"components": [
            {"type": "container", "components": [{"type": "text", "content": "x", "colour": 1}]}
        ]}}}),
        std::path::Path::new("missing-config.json"),
    )
    .await;
    assert_eq!(result["is_error"], true);
    let content = result["content"].as_str().unwrap();
    assert!(
        content.starts_with("Not sent: invalid document"),
        "{content}"
    );
    assert!(
        content.contains("$.components[0].components[0]"),
        "{content}"
    );
    assert!(content.contains("colour"), "{content}");
}

#[tokio::test]
async fn valid_documents_get_past_validation() {
    // With no config the send cannot happen, but the failure must be the
    // delivery error, proving the document itself was accepted.
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_send_ui", "args": {"document": [{"type": "text_display", "content": "hi"}]}}),
        std::path::Path::new("missing-config.json"),
    )
    .await;
    assert_eq!(result["is_error"], true);
    assert!(result["content"]
        .as_str()
        .unwrap()
        .starts_with("Discord delivery failed"));
}

#[tokio::test]
async fn every_tool_reply_carries_content() {
    for args in [json!({"surface": "message"}), json!({"surface": "modal"})] {
        let result = sidecar::dispatch(
            "tool/call",
            &json!({"name": "discord_ui_schema", "args": args}),
            std::path::Path::new("config.json"),
        )
        .await;
        assert!(result["content"].is_string());
    }
}
