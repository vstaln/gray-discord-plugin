mod common;

use gray_discord::command_dispatch::{self, Ctx};
use gray_discord::durable::Store;
use gray_discord::transport::Rest;
use serde_json::json;

fn context<'a>(
    store: &'a Store,
    rest: &'a Rest,
    path: &'a std::path::Path,
    user: &'a str,
    config: &'a serde_json::Value,
) -> Ctx<'a> {
    Ctx {
        user_id: user,
        channel_id: "42",
        is_dm: false,
        int_id: "interaction-1",
        int_token: "interaction-token",
        app_id: "999",
        rest,
        store,
        config,
        config_path: path,
    }
}

#[tokio::test]
async fn cron_button_state_is_opaque_bound_and_single_use() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    store
        .schedule_add("job-1", 3600, "check", "42", "chat:42", 0.0)
        .unwrap();
    let token = store
        .component_state_create("cron_remove", "owner", "42", "job-1", 3600)
        .unwrap();
    assert!(!token.contains("job-1"));

    let config = json!({});
    let ctx = context(&store, &rest, &path, "owner", &config);
    command_dispatch::button(&ctx, &format!("cron:remove:{token}")).await;
    assert!(store.schedules().unwrap().is_empty());

    let sent = stub.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].path,
        "/api/v10/interactions/interaction-1/interaction-token/callback"
    );
    assert_eq!(sent[0].body["type"], json!(7));
    assert_eq!(sent[0].body["data"]["flags"], json!(32768));
    assert!(sent[0].body["data"]["components"][0]["type"]
        .as_u64()
        .is_some());
    assert!(sent[0].body["data"].get("embeds").is_none());
}

#[tokio::test]
async fn cron_button_from_another_user_cannot_remove_the_job() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    store
        .schedule_add("job-1", 3600, "check", "42", "chat:42", 0.0)
        .unwrap();
    let token = store
        .component_state_create("cron_remove", "owner", "42", "job-1", 3600)
        .unwrap();
    let config = json!({});
    let ctx = context(&store, &rest, &path, "intruder", &config);
    command_dispatch::button(&ctx, &format!("cron:remove:{token}")).await;
    assert_eq!(store.schedules().unwrap().len(), 1);
}
