use gray_discord::component_state::{NewDocument, NewEvent, NewState};
use gray_discord::durable::Store;
use serde_json::json;
use tempfile::tempdir;

fn store() -> (Store, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let store = Store::new(&dir.path().join("queue.sqlite3")).expect("store");
    (store, dir)
}

fn document(store: &Store) {
    store
        .create_document(NewDocument {
            document_id: "doc-1".into(),
            owner_id: "user-1".into(),
            guild_id: Some("guild-1".into()),
            channel_id: "channel-1".into(),
            message_id: Some("message-1".into()),
            modal_id: None,
            surface: "message".into(),
            revision: 1,
            protocol_version: 1,
            ttl_secs: 3600,
        })
        .expect("document");
}

#[test]
fn event_is_typed_queued_once_and_duplicate_is_idempotent() {
    let (store, _dir) = store();
    document(&store);
    let token = store
        .create_state(NewState {
            document_id: "doc-1".into(),
            logical_id: "refresh".into(),
            action: "button".into(),
            user_id: "user-1".into(),
            channel_id: "channel-1".into(),
            state: json!({"revision": 1}),
            one_shot: true,
            ttl_secs: 3600,
        })
        .expect("state");

    let request = NewEvent {
        interaction_id: "interaction-1".into(),
        token: token.clone(),
        user_id: "user-1".into(),
        channel_id: "channel-1".into(),
        kind: "button".into(),
        payload: json!({"action": "refresh"}),
        conversation: "chat:channel-1".into(),
        interaction_token: Some("opaque-interaction-token".into()),
        app_id: Some("app-1".into()),
        capacity: 16,
    };
    let first = store.accept_event(request.clone()).expect("first event");
    assert!(first.queued);
    assert!(!first.duplicate);

    let item = store.claim().expect("claim").expect("queued item");
    let input: serde_json::Value =
        serde_json::from_str(item.input_json.as_deref().expect("typed json")).expect("json");
    assert_eq!(input["protocol"], "gray.discord.input");
    assert_eq!(input["kind"], "component_event");
    assert_eq!(input["payload"]["component"], "refresh");
    assert!(!serde_json::to_string(&input).unwrap().contains(&token));

    let duplicate = store.accept_event(request).expect("duplicate event");
    assert!(duplicate.duplicate);
    assert!(store.claim().expect("claim duplicate").is_none());
}

#[test]
fn wrong_user_and_invalidated_state_fail_closed() {
    let (store, _dir) = store();
    document(&store);
    let token = store
        .create_state(NewState {
            document_id: "doc-1".into(),
            logical_id: "select".into(),
            action: "string_select".into(),
            user_id: "user-1".into(),
            channel_id: "channel-1".into(),
            state: json!({}),
            one_shot: false,
            ttl_secs: 3600,
        })
        .expect("state");

    let wrong = store.accept_event(NewEvent {
        interaction_id: "wrong-user".into(),
        token: token.clone(),
        user_id: "user-2".into(),
        channel_id: "channel-1".into(),
        kind: "string_select".into(),
        payload: json!({"values": ["a"]}),
        conversation: "chat:channel-1".into(),
        interaction_token: Some("opaque-interaction-token".into()),
        app_id: Some("app-1".into()),
        capacity: 16,
    });
    assert!(wrong.is_err());

    store.invalidate_document("doc-1").expect("invalidate");
    assert!(store.resolve_state(&token, "user-1", "channel-1").is_err());
    let invalid = store.accept_event(NewEvent {
        interaction_id: "invalidated".into(),
        token,
        user_id: "user-1".into(),
        channel_id: "channel-1".into(),
        kind: "string_select".into(),
        payload: json!({"values": ["a"]}),
        conversation: "chat:channel-1".into(),
        interaction_token: Some("opaque-interaction-token".into()),
        app_id: Some("app-1".into()),
        capacity: 16,
    });
    assert!(invalid.is_err());
}

#[test]
fn state_resolution_is_bound_to_user_channel_and_expiry() {
    let (store, _dir) = store();
    document(&store);
    let token = store
        .create_state(NewState {
            document_id: "doc-1".into(),
            logical_id: "check".into(),
            action: "checkbox".into(),
            user_id: "user-1".into(),
            channel_id: "channel-1".into(),
            state: json!({"checked": true}),
            one_shot: false,
            ttl_secs: 60,
        })
        .expect("state");
    assert!(store.resolve_state(&token, "user-2", "channel-1").is_err());
    assert!(store.resolve_state(&token, "user-1", "channel-2").is_err());
    let resolved = store
        .resolve_state(&token, "user-1", "channel-1")
        .expect("resolve");
    assert_eq!(resolved.logical_id, "check");
    assert_eq!(resolved.state["checked"], true);
}

#[test]
fn autocomplete_is_typed_and_does_not_consume_one_shot_state() {
    let (store, _dir) = store();
    document(&store);
    let token = store
        .create_state(NewState {
            document_id: "doc-1".into(),
            logical_id: "project".into(),
            action: "string_select".into(),
            user_id: "user-1".into(),
            channel_id: "channel-1".into(),
            state: json!({"options": [{"label": "Gray", "value": "gray"}]}),
            one_shot: true,
            ttl_secs: 3600,
        })
        .expect("state");
    let request = NewEvent {
        interaction_id: "autocomplete-1".into(),
        token: token.clone(),
        user_id: "user-1".into(),
        channel_id: "channel-1".into(),
        kind: "autocomplete".into(),
        payload: json!({"values": {"query": "gr"}}),
        conversation: "chat:channel-1".into(),
        interaction_token: Some("opaque-interaction-token".into()),
        app_id: Some("app-1".into()),
        capacity: 16,
    };
    assert!(
        store
            .accept_event(request)
            .expect("autocomplete event")
            .queued
    );
    let item = store.get("component:autocomplete-1").unwrap().unwrap();
    let input: serde_json::Value =
        serde_json::from_str(item.input_json.as_deref().unwrap()).unwrap();
    assert_eq!(input["payload"]["values"]["query"], "gr");
    assert!(store.resolve_state(&token, "user-1", "channel-1").is_ok());
}
