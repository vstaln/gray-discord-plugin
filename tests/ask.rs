//! Questions on Discord: a plugin asks through gray's `host/ask`, the
//! bridge shows a card, and the answers go back by question id.

mod common;

use gray_discord::durable::Store;
use gray_discord::transport::Rest;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// What an `ask` row carries: gray-questions' question shape.
fn questions() -> Vec<gray_discord::ask::Question> {
    gray_discord::ask::from_host(&json!([{
        "id": "branch", "header": "Branch", "question": "Which branch should I deploy?",
        "options": [{"label": "main", "description": "production"}, {"label": "staging"}]
    }]))
}

/// The question card's custom ids, from the stub's first POST.
async fn posted_card(stub: &common::Stub) -> Value {
    loop {
        let card = stub
            .sent
            .lock()
            .unwrap()
            .iter()
            .find(|sent| sent.method == "POST" && sent.path.ends_with("/channels/42/messages"))
            .map(|sent| sent.body["components"].clone());
        if let Some(card) = card {
            return card;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn custom_ids(card: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(id) = map.get("custom_id").and_then(Value::as_str) {
                    out.push(id.to_string());
                }
                map.values().for_each(|child| walk(child, out));
            }
            Value::Array(items) => items.iter().for_each(|child| walk(child, out)),
            _ => {}
        }
    }
    walk(card, &mut out);
    out
}

fn callbacks(stub: &common::Stub) -> Vec<Value> {
    stub.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sent| sent.path.ends_with("/callback"))
        .map(|sent| sent.body.clone())
        .collect()
}

/// A press from an admitted user in channel 42.
macro_rules! press {
    ($stub:expr, $store:expr, $custom_id:expr, $values:expr, $typed:expr) => {{
        let rest = Rest::new(&$stub.base, "TESTTOKEN");
        let config = json!({});
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let ctx = gray_discord::command_dispatch::Ctx {
            user_id: "777",
            channel_id: "42",
            is_dm: true,
            int_id: "5",
            int_token: "press",
            app_id: "999",
            rest: &rest,
            store: &$store,
            config: &config,
            config_path: &config_path,
        };
        gray_discord::command_dispatch::ask_press(&ctx, $custom_id, $values, $typed).await;
    }};
}

fn store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    (tmp, store)
}

#[tokio::test]
async fn a_button_press_answers_and_the_agent_gets_it() {
    let stub = common::Stub::start().await;
    let (_tmp, store) = store();
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let qs = questions();
    let over = AtomicBool::new(false);
    let asking = gray_discord::ask::ask(
        &rest,
        &store,
        42,
        &qs,
        Duration::from_secs(30),
        Duration::from_millis(20),
        &over,
    );
    let answering = async {
        let card = posted_card(&stub).await;
        assert_eq!(card[0]["type"], 17, "a V2 card");
        assert!(!card.to_string().contains("emoji"), "plain text");
        let ids = custom_ids(&card);
        let staging = ids.iter().find(|id| id.ends_with(":0:1")).unwrap().clone();
        press!(stub, store, &staging, &[], None);
    };
    let (result, ()) = tokio::join!(asking, answering);

    assert_eq!(result, json!({"branch": ["staging"]}));
    let update = &callbacks(&stub)[0];
    assert_eq!(
        update["type"], 7,
        "the card is redrawn in the same response"
    );
    let redrawn = update["data"]["components"].to_string();
    assert!(redrawn.contains("-# Answer: staging"));
    assert!(redrawn.contains("-# Answered"));
    assert!(!redrawn.contains("ask:"), "no controls left");
}

#[tokio::test]
async fn other_opens_a_text_box_whose_answer_comes_back_typed() {
    let stub = common::Stub::start().await;
    let (_tmp, store) = store();
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let qs = questions();
    let over = AtomicBool::new(false);
    let asking = gray_discord::ask::ask(
        &rest,
        &store,
        42,
        &qs,
        Duration::from_secs(30),
        Duration::from_millis(20),
        &over,
    );
    let answering = async {
        let card = posted_card(&stub).await;
        let other = custom_ids(&card)
            .into_iter()
            .find(|id| id.ends_with(":other"))
            .unwrap();
        press!(stub, store, &other, &[], None);
        let modal = callbacks(&stub)[0].clone();
        assert_eq!(modal["type"], 9, "a modal");
        assert_eq!(modal["data"]["components"][0]["type"], 18, "a Label");
        let note = modal["data"]["custom_id"].as_str().unwrap().to_string();
        press!(stub, store, &note, &[], Some("the hotfix branch"));
    };
    let (result, ()) = tokio::join!(asking, answering);
    assert_eq!(result, json!({"branch": ["user_note: the hotfix branch"]}));
}

#[tokio::test]
async fn a_plain_reply_in_the_channel_answers_too() {
    let stub = common::Stub::start().await;
    let (_tmp, store) = store();
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let qs = questions();
    let over = AtomicBool::new(false);
    let asking = gray_discord::ask::ask(
        &rest,
        &store,
        42,
        &qs,
        Duration::from_secs(30),
        Duration::from_millis(20),
        &over,
    );
    let replying = async {
        posted_card(&stub).await;
        assert!(gray_discord::ask::answer_typed(&store, &rest, "42", "deploy main please").await);
        assert!(
            !gray_discord::ask::answer_typed(&store, &rest, "42", "a new request").await,
            "once answered, messages start turns again"
        );
    };
    let (result, ()) = tokio::join!(asking, replying);
    assert_eq!(result, json!({"branch": ["user_note: deploy main please"]}));
    let edited = stub
        .sent
        .lock()
        .unwrap()
        .iter()
        .find(|sent| sent.method == "PATCH")
        .map(|sent| sent.body["components"].to_string())
        .unwrap();
    assert!(edited.contains("-# Answer: deploy main please"));
}

#[tokio::test]
async fn no_answer_in_time_closes_the_card() {
    let stub = common::Stub::start().await;
    let (_tmp, store) = store();
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let result = gray_discord::ask::ask(
        &rest,
        &store,
        42,
        &questions(),
        Duration::from_millis(300),
        Duration::from_millis(20),
        &AtomicBool::new(false),
    )
    .await;
    assert_eq!(result, json!({}), "no answers: the plugin decides");
    let closed = stub
        .sent
        .lock()
        .unwrap()
        .iter()
        .find(|sent| sent.method == "PATCH")
        .map(|sent| sent.body["components"].to_string())
        .unwrap();
    assert!(closed.contains("No answer in time"));
    assert!(!closed.contains("ask:"), "the buttons are gone");
    // A late press is told the question closed.
    let late = custom_ids(&posted_card(&stub).await)[0].clone();
    press!(stub, store, &late, &[], None);
    assert_eq!(callbacks(&stub)[0]["type"], 4);
}

#[tokio::test]
async fn the_turn_ending_closes_an_open_card() {
    let stub = common::Stub::start().await;
    let (_tmp, store) = store();
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    let over = AtomicBool::new(true);
    let result = gray_discord::ask::ask(
        &rest,
        &store,
        42,
        &questions(),
        Duration::from_secs(30),
        Duration::from_millis(20),
        &over,
    )
    .await;
    assert_eq!(result, json!({}));
    let closed = stub
        .sent
        .lock()
        .unwrap()
        .iter()
        .find(|sent| sent.method == "PATCH")
        .map(|sent| sent.body["components"].to_string())
        .unwrap();
    assert!(!closed.contains("ask:"), "the buttons are gone");
}

#[test]
fn malformed_questions_show_nothing() {
    assert!(gray_discord::ask::from_host(&json!(null)).is_empty());
    assert!(gray_discord::ask::from_host(&json!([{"id": "", "question": "q"}])).is_empty());
    assert!(gray_discord::ask::from_host(&json!([{"id": "a", "question": "  "}])).is_empty());
}

#[tokio::test]
async fn the_bridge_offers_no_question_tool_of_its_own() {
    let manifest = gray_discord::sidecar::dispatch(
        "plugin/manifest",
        &json!({}),
        std::path::Path::new("config.json"),
    )
    .await;
    let tools = manifest["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .all(|tool| !tool["name"].as_str().unwrap_or_default().contains("ask")),
        "questions come from a questions plugin, not the bridge"
    );
    assert!(tools.iter().all(|tool| tool["label"].is_string()));
}
