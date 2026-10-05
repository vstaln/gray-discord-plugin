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
    // Agnostic transcript previews: gray core resolves the declared dot
    // path against the call args; the plugin never teaches core its shape.
    assert_eq!(send["preview"], "document.title");
    let modal = tools
        .iter()
        .find(|tool| tool["name"] == "discord_open_modal")
        .unwrap();
    assert_eq!(modal["preview"], "document.title");
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
    // The refusal must never send the model to doctor or echo config.
    assert!(!content.contains("doctor"), "{content}");
    assert!(!content.contains("test-token"), "{content}");
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
}

/// Regression: a file import used to hide the managed id in a field the model
/// never reads, so no document could reference it. The id rides `content`.
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
            "gray_bin": std::env::current_exe().unwrap(),
            "gray_home": dir.path().join("gray-home"),
            "workdir": &work,
        })
        .to_string(),
    )
    .expect("config");
    std::fs::write(work.join("slide.png"), b"\x89PNG\r\n\x1a\n").expect("png");

    // The import root is the process cwd — save and restore it.
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&work).expect("chdir");
    let result = sidecar::dispatch(
        "tool/call",
        &json!({"name": "discord_file", "args": {"action": "import", "path": "slide.png"}}),
        &config,
    )
    .await;
    std::env::set_current_dir(previous).expect("restore cwd");

    let content = result["content"].as_str().expect("import reports content");
    assert!(content.contains("file_id"), "content: {content}");
    assert_eq!(result["file"]["name"], "slide.png");
}
