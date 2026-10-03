//! Live replies, Hermes-style: gray's streamed `text` rows -> one Discord
//! message edited in place, then settled as the durable answer.

mod common;

use gray_discord::activity;
use gray_discord::durable::Store;
use gray_discord::gateway::Runtime;
use gray_discord::runner::{RunError, RunInput};
use gray_discord::transport::Rest;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn component_text(value: &Value) -> String {
    match value {
        Value::Array(items) => items.iter().map(component_text).collect(),
        Value::Object(map) => map
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| map.get("components").map(component_text))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn text(segment: u64, delta: &str, tail: &str, done: bool) -> Value {
    json!({"type": "progress", "phase": "text", "segment": segment,
           "delta": delta, "tail": tail, "done": done})
}

fn tool(phase: &str, id: &str, detail: &str) -> Value {
    json!({"type": "progress", "phase": phase, "call_id": id, "tool": "bash", "detail": detail})
}

/// Scripted gray: each step pushes its rows into the sink, advances the
/// clock past the edit gap, and waits for the gateway's 250ms frame.
type Script = Vec<Vec<Value>>;

/// A runtime against the loopback Discord stub whose runner replays
/// `script` and then answers `answer`.
macro_rules! scripted {
    ($config:expr, $stub:expr, $script:expr, $answer:expr) => {{
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
        let _keep = Box::leak(Box::new(tmp));
        let sink = activity::sink();
        let clock = Arc::new(Mutex::new(0.0_f64));
        let script: Arc<Script> = Arc::new($script);
        let answer: String = $answer.to_string();
        let runner_sink = sink.clone();
        let runner_clock = clock.clone();
        let runner = move |_c: &Value, _p: &std::path::Path, conv: &str, _i: &RunInput| {
            let sink = runner_sink.clone();
            let clock = runner_clock.clone();
            let script = script.clone();
            let answer = answer.clone();
            let conv = conv.to_string();
            Box::pin(async move {
                for step in script.iter() {
                    for row in step {
                        activity::push_for(&sink, &conv, row.clone());
                    }
                    *clock.lock().unwrap() += 5.0;
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
                Ok::<String, RunError>(answer)
            })
        };
        let deliver = move |_p: gray_discord::durable::OutboxPart| {
            Box::pin(async move { Ok::<String, String>("delivered".into()) })
        };
        let t = clock.clone();
        let rt = Runtime::new($config, path, store.clone(), deliver, runner)
            .with_activity(sink)
            .with_clock(Arc::new(move || *t.lock().unwrap()))
            .with_rest(Rest::new(&$stub.base, "TESTTOKEN"));
        (rt, store)
    }};
}

/// Message posts and edits the stub saw, as (method, text).
fn traffic(stub: &common::Stub) -> Vec<(String, String)> {
    stub.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sent| sent.path.starts_with("/api/v10/channels/42/messages"))
        .map(|sent| {
            (
                sent.method.clone(),
                component_text(&sent.body["components"]),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_reply_streams_in_place_and_lands_as_the_answer() {
    let stub = common::Stub::start().await;
    let script = vec![
        vec![text(0, "", "Hello", false)],
        vec![text(0, "Hello there,\n", "how", false)],
    ];
    let (rt, store) = scripted!(json!({}), stub, script, "Hello there,\nhow are you?");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    assert_eq!(log[0], ("POST".into(), "Hello ▉".into()), "{log:?}");
    assert!(
        log.contains(&("PATCH".into(), "Hello there,\nhow ▉".into())),
        "the preview grows in place: {log:?}"
    );
    assert_eq!(
        log.last().unwrap(),
        &("PATCH".into(), "Hello there,\nhow are you?".into()),
        "the final edit is the answer, cursor gone: {log:?}"
    );
    assert_eq!(
        log.iter().filter(|(method, _)| method == "POST").count(),
        1,
        "one message for the whole reply: {log:?}"
    );
    let sent = stub.sent.lock().unwrap().clone();
    assert_eq!(sent[0].body["flags"], json!(32768), "Components V2");
    assert!(sent[0].body.get("embeds").is_none());

    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
    assert!(
        store.next_delivery(f64::MAX).unwrap().is_none(),
        "a streamed answer is never posted a second time"
    );
}

#[tokio::test]
async fn prose_and_tool_bubbles_alternate_in_channel_order() {
    let stub = common::Stub::start().await;
    let script = vec![
        vec![
            text(0, "Let me check.", "", true),
            tool("tool_ran", "a", "cargo test"),
        ],
        vec![tool("tool_finished", "a", ""), text(1, "", "All", false)],
    ];
    let (rt, store) = scripted!(json!({}), stub, script, "All green.");
    store.enqueue("m1", "42", "run it", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let posts: Vec<String> = traffic(&stub)
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .map(|(_, body)| body)
        .collect();
    assert_eq!(posts.len(), 3, "{posts:?}");
    assert_eq!(posts[0], "Let me check.");
    assert!(posts[1].starts_with("💻 Running `cargo test`"), "{posts:?}");
    assert_eq!(posts[2], "All ▉");
    let log = traffic(&stub);
    assert_eq!(log.last().unwrap(), &("PATCH".into(), "All green.".into()));
    assert!(
        log.iter().all(|(_, body)| !body.contains("gray ·")),
        "no embed-style cards: {log:?}"
    );
    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
}

#[tokio::test]
async fn a_gray_without_text_rows_still_gets_one_durable_post() {
    let stub = common::Stub::start().await;
    let script = vec![vec![tool("tool_ran", "a", "cargo test")]];
    let (rt, store) = scripted!(json!({}), stub, script, "answer");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    assert_eq!(store.get("m1").unwrap().unwrap().state, "delivery");
    let part = store.next_delivery(f64::MAX).unwrap().unwrap();
    assert_eq!(part.content, "answer");
}

#[tokio::test]
async fn streaming_off_ignores_prose_rows() {
    let stub = common::Stub::start().await;
    let script = vec![vec![text(0, "", "draft", false)]];
    let (rt, store) = scripted!(json!({"stream_replies": false}), stub, script, "answer");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    assert!(traffic(&stub).is_empty(), "{:?}", traffic(&stub));
    let part = store.next_delivery(f64::MAX).unwrap().unwrap();
    assert_eq!(part.content, "answer");
}

#[tokio::test]
async fn a_slash_turn_answers_through_its_interaction_not_the_stream() {
    let stub = common::Stub::start().await;
    let script = vec![vec![text(0, "", "draft", false)]];
    let (rt, store) = scripted!(json!({}), stub, script, "answer");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    store.set_interaction("m1", "token", "999").unwrap();
    assert!(rt.generate_one().await.unwrap());

    assert!(traffic(&stub).is_empty(), "{:?}", traffic(&stub));
    assert!(store.next_delivery(f64::MAX).unwrap().is_some());
}

#[tokio::test]
async fn media_streams_as_prose_and_only_the_files_wait_in_the_outbox() {
    let stub = common::Stub::start().await;
    let files = tempfile::tempdir().unwrap();
    let chart = files.path().join("chart.png");
    std::fs::write(&chart, b"\x89PNG fixture").unwrap();
    let canonical = chart.canonicalize().unwrap();
    let answer = format!("Here it is:\nMEDIA:{}", chart.display());
    let script = vec![vec![text(0, "Here it is:\n", "", false)]];
    let config = json!({"workdir": files.path().to_str().unwrap()});
    let (rt, store) = scripted!(config, stub, script, answer);
    store.enqueue("m1", "42", "chart", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    assert_eq!(log.last().unwrap().1, "Here it is:", "{log:?}");
    assert!(
        log.iter().all(|(_, body)| !body.contains("MEDIA:")),
        "{log:?}"
    );
    assert_eq!(store.get("m1").unwrap().unwrap().state, "delivery");
    let part = store.next_delivery(f64::MAX).unwrap().unwrap();
    assert_eq!(part.part, 1, "the prose part is already delivered");
    assert_eq!(part.content, format!("MEDIA:{}", canonical.display()));
}

#[tokio::test]
async fn a_refused_preview_falls_back_to_one_durable_post() {
    let stub = common::Stub::start().await;
    *stub.fail_send.lock().unwrap() = true;
    let script = vec![
        vec![text(0, "", "Hello", false)],
        vec![text(0, "Hello there\n", "", false)],
    ];
    let (rt, store) = scripted!(json!({}), stub, script, "Hello there");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let posts = traffic(&stub)
        .into_iter()
        .filter(|(method, _)| method == "POST")
        .count();
    assert_eq!(posts, 1, "a refused preview is not retried every frame");
    assert_eq!(store.get("m1").unwrap().unwrap().state, "delivery");
    let part = store.next_delivery(f64::MAX).unwrap().unwrap();
    assert_eq!(
        part.content, "Hello there",
        "the outbox still owns the answer"
    );
}
