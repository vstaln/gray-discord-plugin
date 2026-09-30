mod common;

use gray_discord::transport::{Rest, TransportError};
use serde_json::Value;

fn component_text(value: &Value) -> String {
    match value {
        Value::Array(items) => items.iter().map(component_text).collect(),
        Value::Object(map) => {
            if let Some(text) = map.get("content").and_then(Value::as_str) {
                text.to_string()
            } else {
                map.get("components")
                    .map(component_text)
                    .unwrap_or_default()
            }
        }
        _ => String::new(),
    }
}

#[tokio::test]
async fn sdk_sends_split_text_without_mentions() {
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let text = "😀".repeat(1100) + "@everyone";
    let first = rest.rest_send("42", &text).await.expect("send");
    assert_eq!(first, "1000");
    let sent = stub.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    let joined: String = sent
        .iter()
        .map(|s| component_text(&s.body["components"]))
        .collect();
    assert_eq!(joined, text);
    for s in &sent {
        assert_eq!(s.method, "POST");
        assert_eq!(s.path, "/api/v10/channels/42/messages");
        assert_eq!(s.auth.as_deref(), Some("Bot TESTTOKEN"));
        assert!(s
            .headers
            .get("user-agent")
            .is_some_and(|v| v.contains("gray-discord")));
        assert_eq!(s.body["flags"], serde_json::json!(32768));
        assert_eq!(s.body["allowed_mentions"]["parse"], serde_json::json!([]));
        assert!(s.body.get("content").is_none());
        assert!(s.body.get("embeds").is_none());
    }
}

#[tokio::test]
async fn auth_failure_is_not_success() {
    let stub = common::Stub::start().await;
    *stub.fail_auth.lock().unwrap() = true;
    let rest = Rest::new(&stub.base, "BADTOKEN");
    assert!(matches!(
        rest.rest_send("42", "hello").await,
        Err(TransportError::Auth(_))
    ));
    assert!(stub.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn send_failure_is_not_success() {
    let stub = common::Stub::start().await;
    *stub.fail_send.lock().unwrap() = true;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    assert!(matches!(
        rest.rest_send("42", "hello").await,
        Err(TransportError::Forbidden(_))
    ));
    assert_eq!(stub.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_content_rejected_before_http() {
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    // `None` is a Python-only case (`&str` cannot be null); `""` and
    // whitespace-only both hit the trim-empty branch before any HTTP.
    for bad in ["", "  "] {
        assert!(matches!(
            rest.rest_send("42", bad).await,
            Err(TransportError::Invalid(_))
        ));
    }
    assert!(matches!(
        rest.rest_send("42", &"x".repeat(20001)).await,
        Err(TransportError::Invalid(_))
    ));
    assert!(stub.sent.lock().unwrap().is_empty());
}

#[test]
fn chunk_cap_truncates_at_8_parts_with_notice() {
    let tmp = tempfile::tempdir().unwrap();
    let store = gray_discord::durable::Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    store.enqueue("msg_chunk", "42", "q", None, 100).unwrap();
    let item = store.claim().unwrap().unwrap();
    let text = ("para\n\n".repeat(350) + "\n\n").repeat(9);
    store
        .complete(&item.id, &text, &serde_json::json!({}))
        .unwrap();

    let mut parts = Vec::new();
    while let Some(part) = store.next_delivery(0.0).unwrap() {
        parts.push(part.clone());
        store
            .ack(&part.id, part.part, &format!("m{}", part.part))
            .unwrap();
    }
    assert_eq!(parts.len(), 8);
    assert!(parts[7].content.contains("… (truncated,"));
    assert!(parts[7].content.contains("more characters not sent)"));
}

#[tokio::test]
async fn v2_interactions_use_callback_and_original_response_routes() {
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let components = gray_discord::render::card("Status", "ready", 0x5865F2, &[]).unwrap();

    rest.interaction_v2("interaction", "token", &components, false)
        .await
        .unwrap();
    rest.defer_v2("interaction", "token", true).await.unwrap();
    rest.edit_original_v2("999", "token", &components, true)
        .await
        .unwrap();
    rest.interaction_update_v2("interaction", "token", &components)
        .await
        .unwrap();

    let sent = stub.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 4);
    assert_eq!(
        sent[0].path,
        "/api/v10/interactions/interaction/token/callback"
    );
    assert_eq!(sent[0].body["type"], serde_json::json!(4));
    assert_eq!(sent[0].body["data"]["flags"], serde_json::json!(32832));
    assert_eq!(sent[1].body["type"], serde_json::json!(5));
    assert_eq!(sent[2].method, "PATCH");
    assert_eq!(
        sent[2].path,
        "/api/v10/webhooks/999/token/messages/@original"
    );
    assert_eq!(sent[2].body["flags"], serde_json::json!(32768));
    assert!(sent[2].body["content"].is_null());
    assert_eq!(sent[3].body["type"], serde_json::json!(7));
}

#[tokio::test]
async fn invalid_v2_components_fail_before_http() {
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let bad = vec![serde_json::json!({
        "type": 2,
        "label": "not top-level",
        "style": 1,
        "custom_id": "bad"
    })];
    assert!(matches!(
        rest.send_v2(42, &bad, None).await,
        Err(TransportError::Invalid(_))
    ));
    assert!(stub.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn compiled_transport_covers_messages_interactions_and_multipart() {
    use gray_discord::component_compile::CompiledMessage;
    use gray_discord::component_media::FileStore;
    use gray_discord::component_protocol::FileRef;
    use gray_discord::durable::Store;

    let tmp = tempfile::tempdir().unwrap();
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let compiled = CompiledMessage {
        flags: gray_discord::component_compile::IS_COMPONENTS_V2,
        visibility: gray_discord::component_protocol::Visibility::Public,
        components: vec![serde_json::json!({"type": 10, "content": "typed"})],
    };
    let sent_id = rest
        .send_compiled(
            42,
            &compiled,
            &[],
            &FileStore::new(
                Store::new(&tmp.path().join("unused.sqlite")).unwrap(),
                tmp.path().join("media"),
                tmp.path().join("work"),
            )
            .unwrap(),
            "owner",
            None,
        )
        .await
        .unwrap();
    assert!(!sent_id.is_empty());
    {
        let sent = stub.sent.lock().unwrap();
        assert_eq!(
            sent[0].body["flags"],
            gray_discord::component_compile::IS_COMPONENTS_V2
        );
        assert_eq!(sent[0].body["components"][0]["type"], 10);
        assert!(sent[0].body.get("content").is_none());
    }

    let _ = rest
        .open_modal(
            "i-1",
            "t-1",
            &gray_discord::component_compile::CompiledModal {
                custom_id: "modal-1".into(),
                title: "Input".into(),
                components: vec![],
            },
        )
        .await;
    let _ = rest.defer_update("i-2", "t-2").await;
    let _ = rest.update_message("i-3", "t-3", &compiled).await;
    let _ = rest
        .autocomplete_response("i-4", "t-4", &[("one".into(), "1".into())])
        .await;
    {
        let sent = stub.sent.lock().unwrap();
        assert_eq!(sent[1].body["type"], 9);
        assert_eq!(sent[2].body["type"], 6);
        assert_eq!(sent[3].body["type"], 7);
        assert_eq!(sent[4].body["type"], 8);
    }

    let store = Store::new(&tmp.path().join("files.sqlite")).unwrap();
    let files = FileStore::new(
        store.clone(),
        tmp.path().join("media"),
        tmp.path().join("work"),
    )
    .unwrap();
    let file = files
        .import_bytes("owner", "hello.txt", "text/plain", b"hello")
        .unwrap();
    let file_ref = FileRef {
        id: file.id,
        name: file.name,
        media_type: file.media_type,
        size: file.size,
        sha256: file.sha256,
    };
    let _ = rest
        .send_compiled(42, &compiled, &[file_ref], &files, "owner", None)
        .await
        .unwrap();
    let sent = stub.sent.lock().unwrap();
    let last = sent.last().unwrap();
    assert!(last
        .headers
        .get("content-type")
        .is_some_and(|value| value.starts_with("multipart/form-data")));
    assert_eq!(last.body["components"][0]["type"], 10);
}
