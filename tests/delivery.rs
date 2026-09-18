mod common;

use gray_discord::transport::{Rest, TransportError};

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
        .map(|s| s.body["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(joined, text);
    for s in &sent {
        assert_eq!(s.auth.as_deref(), Some("Bot TESTTOKEN"));
        assert_eq!(s.body["allowed_mentions"]["parse"], serde_json::json!([]));
        assert!(s.body.get("replied_user").is_none());
        assert!(s.body.get("message_reference").is_none());
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
