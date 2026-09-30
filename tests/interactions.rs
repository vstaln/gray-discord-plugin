use gray_discord::component_input::normalize;
use gray_discord::component_state::{NewDocument, NewEvent, NewState};
use gray_discord::durable::Store;
use serde_json::json;

#[test]
fn normalized_button_is_queued_once_without_exposing_state_token() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    store
        .create_document(NewDocument {
            document_id: "doc-1".into(),
            owner_id: "user-1".into(),
            guild_id: Some("guild-1".into()),
            channel_id: "42".into(),
            message_id: Some("message-1".into()),
            modal_id: None,
            surface: "message".into(),
            revision: 1,
            protocol_version: 1,
            ttl_secs: 3600,
        })
        .unwrap();
    let token = store
        .create_state(NewState {
            document_id: "doc-1".into(),
            logical_id: "refresh".into(),
            action: "button".into(),
            user_id: "user-1".into(),
            channel_id: "42".into(),
            state: json!({"revision": 1}),
            one_shot: true,
            ttl_secs: 3600,
        })
        .unwrap();
    let raw = json!({
        "id": "interaction-1",
        "channel_id": "42",
        "guild_id": "guild-1",
        "user": {"id": "user-1"},
        "message": {"id": "message-1"},
        "data": {"component_type": 2, "custom_id": token}
    });
    let normalized = normalize(&raw).unwrap();
    assert_eq!(normalized.kind(), "button");
    let event = NewEvent {
        interaction_id: "interaction-1".into(),
        token: normalized.values().state_token.clone(),
        user_id: "user-1".into(),
        channel_id: "42".into(),
        kind: normalized.kind().into(),
        payload: json!({"values": normalized.values().values}),
        conversation: "chat:42:user:user-1".into(),
        interaction_token: Some("discord-token".into()),
        app_id: Some("app-1".into()),
        capacity: 100,
    };
    let receipt = store.accept_event(event.clone()).unwrap();
    assert!(receipt.queued);
    let duplicate = store.accept_event(event).unwrap();
    assert!(duplicate.duplicate);
    let item = store.get("component:interaction-1").unwrap().unwrap();
    let input: serde_json::Value =
        serde_json::from_str(item.input_json.as_deref().unwrap()).unwrap();
    assert_eq!(input["protocol"], "gray.discord.input");
    assert!(!input.to_string().contains(&token));
    assert_eq!(item.interaction_token.as_deref(), Some("discord-token"));
}

#[test]
fn modal_normalization_collects_values_and_attachment_metadata() {
    let raw = json!({
        "id": "modal-interaction",
        "channel_id": "42",
        "user": {"id": "user-1"},
        "data": {
            "custom_id": "state",
            "components": [
                {"type": 4, "custom_id": "name", "value": "Ada"},
                {"type": 19, "custom_id": "upload", "attachments": [
                    {"id": "attachment-1", "url": "https://cdn.discordapp.com/a", "filename": "a.txt", "size": 3}
                ]}
            ]
        }
    });
    let normalized = normalize(&raw).unwrap();
    assert_eq!(normalized.kind(), "modal_submit");
    assert_eq!(normalized.values().values["name"]["value"], "Ada");
    assert_eq!(normalized.values().attachments[0].id, "attachment-1");
}
