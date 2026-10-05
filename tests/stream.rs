//! Live replies, Hermes-style: gray's streamed rows -> a new Discord message
//! per step of the turn (prose, then its tool lines, then the next prose),
//! each edited in place while it is live, the last settled as the durable
//! answer.

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
/// `script` and then answers `answer`. The third value is how long the
/// runner itself took, start to answer.
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
        let took: Arc<Mutex<Option<std::time::Duration>>> = Arc::default();
        let runner_took = took.clone();
        let runner = move |_c: &Value, _p: &std::path::Path, conv: &str, _i: &RunInput| {
            let sink = runner_sink.clone();
            let clock = runner_clock.clone();
            let script = script.clone();
            let answer = answer.clone();
            let conv = conv.to_string();
            let took = runner_took.clone();
            Box::pin(async move {
                let started = std::time::Instant::now();
                for step in script.iter() {
                    for row in step {
                        activity::push_for(&sink, &conv, row.clone());
                    }
                    *clock.lock().unwrap() += 5.0;
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
                *took.lock().unwrap() = Some(started.elapsed());
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
        (rt, store, took)
    }};
}

/// One message request the stub saw.
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    method: String,
    /// The message a PATCH targets (empty for a POST).
    target: String,
    /// The message's Text Displays, joined by newlines (not the chip).
    body: String,
    /// The status chip's line, when this message carries it.
    status: Option<String>,
    accent: Option<u64>,
    stop: Option<String>,
    /// The chip's action-row buttons: (label, custom_id).
    buttons: Vec<(String, String)>,
    /// Gallery and File components in the message.
    media: Vec<Value>,
    /// The upload list a multipart request carried.
    attachments: Vec<String>,
}

/// The status chip is the accented, non-spoiler Container at the end.
fn chip_of(components: &[Value]) -> Option<&Value> {
    components
        .last()
        .filter(|last| last["type"] == 17 && last["spoiler"] != json!(true))
}

/// Message posts and edits the stub saw, in order.
fn traffic(stub: &common::Stub) -> Vec<Frame> {
    stub.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sent| sent.path.starts_with("/api/v10/channels/42/messages"))
        .filter(|sent| sent.method != "DELETE")
        .map(|sent| {
            let components = sent.body["components"].as_array().cloned().unwrap();
            let chip = chip_of(&components).cloned();
            let (status, stop, buttons) = match &chip {
                Some(chip) => {
                    let children = chip["components"].as_array().unwrap();
                    let first = &children[0];
                    let (line, stop) = match first["type"].as_u64() {
                        Some(9) => (
                            component_text(&first["components"]),
                            first["accessory"]["custom_id"].as_str().map(str::to_string),
                        ),
                        _ => (component_text(first), None),
                    };
                    let buttons = children[1..]
                        .iter()
                        .filter(|child| child["type"] == 1)
                        .flat_map(|row| row["components"].as_array().cloned().unwrap_or_default())
                        .map(|button| {
                            (
                                button["label"].as_str().unwrap_or("").to_string(),
                                button["custom_id"].as_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect();
                    (Some(line), stop, buttons)
                }
                None => (None, None, Vec::new()),
            };
            Frame {
                method: sent.method.clone(),
                target: sent
                    .path
                    .strip_prefix("/api/v10/channels/42/messages/")
                    .unwrap_or("")
                    .to_string(),
                body: components
                    .iter()
                    .filter(|child| child["type"] == 10)
                    .map(component_text)
                    .collect::<Vec<_>>()
                    .join("\n"),
                status,
                accent: chip.as_ref().and_then(|chip| chip["accent_color"].as_u64()),
                stop,
                buttons,
                media: components
                    .iter()
                    .filter(|child| child["type"] == 12 || child["type"] == 13)
                    .cloned()
                    .collect(),
                attachments: sent.body["attachments"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item["filename"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn posts(log: &[Frame]) -> Vec<&Frame> {
    log.iter().filter(|frame| frame.method == "POST").collect()
}

/// The interaction callbacks the stub saw, as (type, body).
fn callbacks(stub: &common::Stub) -> Vec<Value> {
    stub.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sent| sent.path.ends_with("/callback"))
        .map(|sent| sent.body.clone())
        .collect()
}

/// A button press from an admitted user in channel 42.
macro_rules! press {
    ($stub:expr, $store:expr, $handler:ident, $custom_id:expr $(, $extra:expr)*) => {{
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
        gray_discord::command_dispatch::$handler(&ctx, $custom_id $(, $extra)*).await;
    }};
}

const WORKING: u64 = 0x4E5058;
const DONE: u64 = 0x57F287;
const STOPPED: u64 = 0x80848E;

#[tokio::test]
async fn a_reply_streams_into_its_own_message_and_lands_as_the_answer() {
    let stub = common::Stub::start().await;
    let script = vec![
        vec![text(0, "", "Hello", false)],
        vec![text(0, "Hello there,\n", "how", false)],
    ];
    let (rt, store, _) = scripted!(json!({}), stub, script, "Hello there,\nhow are you?");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    assert_eq!(log[0].method, "POST");
    assert_eq!(log[0].body, "Hello ▉", "{log:?}");
    assert_eq!(
        log[0].status.as_deref(),
        Some("-# Working · started <t:0:R>"),
        "a live Discord timestamp, not a ticking edit"
    );
    assert_eq!(log[0].accent, Some(WORKING));
    assert!(
        log[0]
            .stop
            .as_deref()
            .is_some_and(|id| id.starts_with("turn:stop:")),
        "a live turn offers Stop: {log:?}"
    );
    assert!(
        log.iter()
            .any(|frame| frame.method == "PATCH" && frame.body == "Hello there,\nhow ▉"),
        "the prose grows in place: {log:?}"
    );
    let last = log.last().unwrap();
    assert_eq!(last.method, "PATCH");
    assert_eq!(last.body, "Hello there,\nhow are you?", "cursor gone");
    assert!(
        last.status.as_deref().unwrap().starts_with("-# Done in "),
        "{last:?}"
    );
    assert_eq!(last.accent, Some(DONE));
    assert_eq!(last.stop, None, "Stop goes away once the turn is over");
    let labels: Vec<&str> = last
        .buttons
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    assert_eq!(labels, vec!["Retry", "New chat"], "the settled actions");
    assert_eq!(
        posts(&log).len(),
        1,
        "one run of prose, one message: {log:?}"
    );
    let sent = stub.sent.lock().unwrap().clone();
    assert_eq!(sent[0].body["flags"], json!(32768), "Components V2");
    assert!(sent[0].body.get("embeds").is_none());
    let edit = sent.iter().find(|sent| sent.method == "PATCH").unwrap();
    assert_eq!(edit.body["flags"], json!(32768));
    for legacy in ["content", "embeds", "sticker_ids"] {
        assert!(
            edit.body.get(legacy).is_none(),
            "an edit is shaped like the post: {legacy}"
        );
    }

    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
    assert!(
        store.next_delivery(f64::MAX).unwrap().is_none(),
        "a streamed answer is never posted a second time"
    );
}

#[tokio::test]
async fn each_step_of_the_turn_is_a_new_message() {
    let stub = common::Stub::start().await;
    let script = vec![
        vec![text(0, "Let me check.", "", false)],
        vec![text(0, "", "", true), tool("tool_ran", "a", "cargo test")],
        vec![tool("tool_finished", "a", ""), text(1, "", "All", false)],
    ];
    let (rt, store, _) = scripted!(json!({}), stub, script, "All green.");
    store.enqueue("m1", "42", "run it", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    let posted: Vec<&str> = posts(&log)
        .iter()
        .map(|frame| frame.body.as_str())
        .collect();
    assert_eq!(
        posted,
        vec!["Let me check. ▉", "-# Running `cargo test`", "All ▉"],
        "prose, its tool lines, then the next prose, each a new message: {log:?}"
    );
    // The chip rides the newest message: the older ones lose it.
    let ids: Vec<String> = (0..3).map(|n| format!("{}", 1000 + n)).collect();
    let first_settled = log
        .iter()
        .filter(|frame| frame.method == "PATCH")
        .find(|frame| frame.body == "Let me check.")
        .expect("the first prose closes: {log:?}");
    assert_eq!(first_settled.status, None, "{log:?}");
    assert!(ids.contains(&first_settled.target), "{log:?}");
    let tools = log
        .iter()
        .rev()
        .find(|frame| frame.body.starts_with("-# Ran `cargo test` ("))
        .expect("the tool line gets its duration");
    assert_eq!(tools.status, None, "{tools:?}");
    let last = log.last().unwrap();
    assert_eq!(last.body, "All green.");
    assert!(
        last.status
            .as_deref()
            .unwrap()
            .ends_with(" · ran 1 command"),
        "the chip tallies the turn: {last:?}"
    );
    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
}

#[tokio::test]
async fn the_stop_button_flips_the_message_at_once_and_the_turn_stops() {
    let stub = common::Stub::start().await;
    let script = vec![
        vec![text(0, "", "Working on", false)],
        vec![],
        vec![],
        vec![],
        vec![],
    ];
    let (rt, store, _) = scripted!(json!({}), stub, script, "never");
    store.enqueue("m1", "42", "long job", None, 1000).unwrap();
    let press = async {
        let (stop, pressed) = loop {
            let first = stub
                .sent
                .lock()
                .unwrap()
                .iter()
                .find(|sent| sent.method == "POST" && sent.path.ends_with("/messages"))
                .map(|sent| sent.body["components"].clone());
            if let Some(components) = first {
                let chip = chip_of(components.as_array().unwrap()).unwrap().clone();
                let stop = chip["components"][0]["accessory"]["custom_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                break (stop, components);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        press!(stub, store, stop_button, &stop, Some(&pressed));
    };
    let (generated, ()) = tokio::join!(rt.generate_one(), press);
    assert!(generated.unwrap());

    let callback = callbacks(&stub).into_iter().next().expect("acknowledged");
    assert_eq!(
        callback["type"], 7,
        "the pressed message updates in the same response"
    );
    let flipped = callback["data"]["components"].to_string();
    assert!(flipped.contains("Stopping…"), "{flipped}");
    assert!(
        !flipped.contains("turn:stop:"),
        "the button is gone at once"
    );
    let flipped_chip = chip_of(callback["data"]["components"].as_array().unwrap()).unwrap();
    assert_eq!(flipped_chip["accent_color"], json!(STOPPED));

    let last = traffic(&stub).last().cloned().unwrap();
    assert_eq!(last.accent, Some(STOPPED));
    assert_eq!(last.body, "Working on", "cursor gone");
    let status = last.status.unwrap();
    assert!(status.starts_with("-# Stopped after "), "{status}");
    assert!(status.ends_with("actions may already have happened"));
    let item = store.get("m1").unwrap().unwrap();
    assert_eq!(item.error.as_deref(), Some("cancelled"));
    assert!(
        store.next_delivery(f64::MAX).unwrap().is_none(),
        "the chip is the notice; nothing else is posted"
    );
}

#[tokio::test]
async fn retry_queues_the_same_prompt_again() {
    let stub = common::Stub::start().await;
    let script = vec![vec![text(0, "", "Hi", false)]];
    let (rt, store, _) = scripted!(json!({}), stub, script, "Hi there");
    store.enqueue("m1", "42", "say hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());
    let (_, retry) = traffic(&stub).last().unwrap().buttons[0].clone();
    assert!(retry.starts_with("turn:retry:"));

    press!(stub, store, retry_button, &retry);
    assert_eq!(
        callbacks(&stub)[0]["type"],
        6,
        "the new turn is the feedback"
    );
    let again = store.claim().unwrap().expect("a new turn is queued");
    assert_eq!(again.prompt, "say hi");
    assert!(again.id.starts_with("m1-retry-"));

    press!(stub, store, retry_button, &retry);
    assert_eq!(
        callbacks(&stub)[1]["type"],
        4,
        "a second press says it expired"
    );
}

#[tokio::test]
async fn new_chat_resets_the_conversation_the_turn_belongs_to() {
    let stub = common::Stub::start().await;
    let script = vec![vec![text(0, "", "Hi", false)]];
    let (rt, store, _) = scripted!(json!({}), stub, script, "Hi there");
    store.enqueue("m1", "42", "say hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());
    let (label, new_chat) = traffic(&stub).last().unwrap().buttons[1].clone();
    assert_eq!(label, "New chat");

    press!(stub, store, new_chat_button, &new_chat);
    let reply = &callbacks(&stub)[0];
    assert_eq!(reply["type"], 4);
    assert!(
        reply["data"].to_string().contains("starts fresh"),
        "{reply}"
    );
}

#[tokio::test]
async fn the_next_turn_retires_the_previous_buttons() {
    let stub = common::Stub::start().await;
    let script = vec![vec![text(0, "", "One", false)]];
    let (rt, store, _) = scripted!(json!({}), stub, script, "One");
    store.enqueue("m1", "42", "first", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());
    store.enqueue("m2", "42", "second", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    let retired = log
        .iter()
        .find(|frame| frame.method == "PATCH" && frame.buttons.is_empty() && frame.body == "One")
        .expect("the first turn's last message is edited once more");
    assert_eq!(retired.accent, Some(DONE), "only the buttons go");
    let newest = log.last().unwrap();
    assert_eq!(newest.buttons.len(), 2, "the latest turn keeps its actions");
}

#[tokio::test]
async fn a_gray_without_text_rows_posts_the_answer_below_its_tool_lines() {
    let stub = common::Stub::start().await;
    let script = vec![vec![tool("tool_ran", "a", "cargo test")]];
    let (rt, store, _) = scripted!(json!({}), stub, script, "answer");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    let posted: Vec<&str> = posts(&log)
        .iter()
        .map(|frame| frame.body.as_str())
        .collect();
    assert_eq!(posted, vec!["-# Running `cargo test`", "answer"], "{log:?}");
    let last = log.last().unwrap();
    assert_eq!(last.accent, Some(DONE), "the chip moved to the answer");
    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
    assert!(store.next_delivery(f64::MAX).unwrap().is_none());
}

#[tokio::test]
async fn streaming_off_ignores_prose_rows() {
    let stub = common::Stub::start().await;
    let script = vec![vec![text(0, "", "draft", false)]];
    let (rt, store, _) = scripted!(json!({"stream_replies": false}), stub, script, "answer");
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
    let (rt, store, _) = scripted!(json!({}), stub, script, "answer");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    store.set_interaction("m1", "token", "999").unwrap();
    assert!(rt.generate_one().await.unwrap());

    assert!(traffic(&stub).is_empty(), "{:?}", traffic(&stub));
    assert!(store.next_delivery(f64::MAX).unwrap().is_some());
}

#[tokio::test]
async fn the_answers_files_land_inside_its_message() {
    let stub = common::Stub::start().await;
    let files = tempfile::tempdir().unwrap();
    let chart = files.path().join("chart.png");
    std::fs::write(&chart, b"\x89PNG fixture").unwrap();
    let report = files.path().join("report.pdf");
    std::fs::write(&report, b"%PDF fixture").unwrap();
    let answer = format!(
        "Here it is:\nMEDIA:{}\nMEDIA:{}",
        chart.display(),
        report.display()
    );
    let script = vec![vec![text(0, "Here it is:\n", "", false)]];
    let config = json!({"workdir": files.path().to_str().unwrap()});
    let (rt, store, _) = scripted!(config, stub, script, answer);
    store.enqueue("m1", "42", "chart", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    assert!(
        log.iter().all(|frame| !frame.body.contains("MEDIA:")),
        "{log:?}"
    );
    let last = log.last().unwrap();
    assert_eq!(last.body, "Here it is:");
    assert_eq!(
        last.attachments,
        vec!["chart.png", "report.pdf"],
        "uploaded with the edit"
    );
    assert_eq!(last.media[0]["type"], 12, "image in a gallery");
    assert_eq!(
        last.media[0]["items"][0]["media"]["url"],
        "attachment://chart.png"
    );
    assert_eq!(last.media[1]["type"], 13, "the PDF as a file card");
    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
    assert!(
        store.next_delivery(f64::MAX).unwrap().is_none(),
        "nothing waits in the outbox: the files are in the message"
    );
}

#[tokio::test]
async fn a_forbidden_channel_falls_back_to_one_durable_post() {
    let stub = common::Stub::start().await;
    *stub.fail_send.lock().unwrap() = true;
    let script = vec![
        vec![text(0, "", "Hello", false)],
        vec![text(0, "Hello there\n", "", false)],
    ];
    let (rt, store, _) = scripted!(json!({}), stub, script, "Hello there");
    store.enqueue("m1", "42", "hi", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    assert_eq!(
        posts(&traffic(&stub)).len(),
        1,
        "a forbidden channel is not retried every frame"
    );
    assert_eq!(store.get("m1").unwrap().unwrap().state, "delivery");
    let part = store.next_delivery(f64::MAX).unwrap().unwrap();
    assert_eq!(
        part.content, "Hello there",
        "the outbox still owns the answer"
    );
}

#[tokio::test]
async fn refused_edits_never_freeze_the_turn() {
    // The bug this guards: one refused edit used to give the whole turn up,
    // so the channel showed its first frame until the answer came, minutes
    // later, as a separate post.
    let stub = common::Stub::start().await;
    *stub.edit_status.lock().unwrap() = Some(400);
    let script = vec![
        vec![text(0, "", "Checking", false)],
        vec![
            text(0, "Checking the build.", "", true),
            tool("tool_ran", "a", "cargo build"),
        ],
        vec![tool("tool_finished", "a", ""), text(1, "", "Built", false)],
    ];
    let (rt, store, _) = scripted!(json!({}), stub, script, "Built fine.");
    store.enqueue("m1", "42", "build", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let log = traffic(&stub);
    let posted: Vec<&str> = posts(&log)
        .iter()
        .map(|frame| frame.body.as_str())
        .collect();
    assert_eq!(
        &posted[..3],
        &["Checking ▉", "-# Running `cargo build`", "Built ▉"],
        "every step still lands as its own message: {log:?}"
    );
    assert!(
        log.iter().filter(|frame| frame.method == "PATCH").count() >= 2,
        "edits keep being tried: {log:?}"
    );
    let deleted: Vec<String> = stub
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sent| sent.method == "DELETE")
        .map(|sent| sent.path.clone())
        .collect();
    let answer = &log.last().unwrap().target;
    assert_eq!(
        deleted,
        vec![format!("/api/v10/channels/42/messages/{answer}")],
        "only the unfinished answer preview is retracted: {log:?}"
    );
    let part = store.next_delivery(f64::MAX).unwrap().unwrap();
    assert_eq!(
        part.content, "Built fine.",
        "and the answer is still delivered"
    );
}

#[tokio::test]
async fn a_slow_discord_never_holds_up_the_agent() {
    // Every edit takes 1.5s. The agent's own steps (3 x 400ms) must not
    // wait on them: gray's output is read beside the requests, not between.
    let stub = common::Stub::start().await;
    *stub.edit_delay_ms.lock().unwrap() = 1500;
    let script = vec![
        vec![text(0, "", "One", false)],
        vec![text(0, "One two\n", "", false)],
        vec![text(0, "One two\nthree\n", "", false)],
    ];
    let (rt, store, took) = scripted!(json!({}), stub, script, "One two\nthree");
    store.enqueue("m1", "42", "count", None, 1000).unwrap();
    assert!(rt.generate_one().await.unwrap());

    let took = took.lock().unwrap().expect("the runner finished");
    assert!(
        took < std::time::Duration::from_millis(1500),
        "the agent waited on Discord: {took:?}"
    );
    let last = traffic(&stub).last().cloned().unwrap();
    assert_eq!(last.body, "One two\nthree");
    assert_eq!(store.get("m1").unwrap().unwrap().state, "sent");
}
