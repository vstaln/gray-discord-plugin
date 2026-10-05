//! Narration: gray's `--json` progress rows -> one status bubble.

mod common;

use gray_discord::activity;
use gray_discord::durable::Store;
use gray_discord::gateway::Runtime;
use gray_discord::transport::Rest;
use serde_json::json;

fn component_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(items) => items.iter().map(component_text).collect(),
        serde_json::Value::Object(map) => map
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| map.get("components").map(component_text))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

// ── rendering (Hermes parity) ──

fn rows(v: serde_json::Value) -> Vec<serde_json::Value> {
    vec![v]
}

#[test]
fn bash_line_matches_hermes_format() {
    let r = rows(json!({"phase": "tool_ran", "tool": "bash", "detail": "ls -la"}));
    assert_eq!(activity::render(&r).as_deref(), Some("Running `ls -la`"));
}

#[test]
fn a_start_line_is_replaced_by_its_own_ran_line() {
    let started = rows(json!({"phase": "tool_started", "call_id": "a", "tool": "bash"}));
    assert_eq!(activity::render(&started).as_deref(), Some("Running"));
    let ran =
        rows(json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "sleep 30"}));
    assert_eq!(
        activity::render(&ran).as_deref(),
        Some("Running `sleep 30`")
    );
    let mut both = started.clone();
    both.extend(ran.clone());
    assert_eq!(
        activity::render(&both).as_deref(),
        Some("Running `sleep 30`"),
        "one call must not cost two lines"
    );
}

#[test]
fn parallel_calls_each_replace_their_own_placeholder() {
    let mut batch: Vec<serde_json::Value> = ["a", "b", "c"]
        .iter()
        .map(|id| json!({"phase": "tool_started", "call_id": id, "tool": "bash"}))
        .collect();
    for (id, command) in [("a", "uptime"), ("b", "date -u"), ("c", "nproc")] {
        batch.push(json!({"phase": "tool_ran", "call_id": id, "tool": "bash", "detail": command}));
    }
    assert_eq!(
        activity::render(&batch).as_deref(),
        Some("Running `uptime`\nRunning `date -u`\nRunning `nproc`"),
        "three calls, three lines, no stale placeholders"
    );
}

#[test]
fn a_line_carries_the_duration_once_the_call_returns() {
    let ran = rows(json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "ls"}));
    assert_eq!(activity::render(&ran).as_deref(), Some("Running `ls`"));
    let mut with_done = ran.clone();
    with_done
        .push(json!({"phase": "tool_finished", "call_id": "a", "tool": "bash", "elapsed_ms": 420}));
    assert_eq!(
        activity::render(&with_done).as_deref(),
        Some("Ran `ls` (0.4s)"),
        "the feed stays in the channel, so a returned call reads as done"
    );
}

#[test]
fn a_chained_command_previews_as_one_command() {
    let r = rows(json!({
        "phase": "tool_ran",
        "tool": "bash",
        "detail": "gray memory list 2>&1; echo \"---PROJECT---\"; gray memory list"
    }));
    assert_eq!(
        activity::render(&r).as_deref(),
        Some("Running `gray memory list … +2`")
    );
}

#[test]
fn a_long_command_is_capped_at_the_preview_length() {
    let detail = "x".repeat(200);
    let r = rows(json!({"phase": "tool_ran", "tool": "bash", "detail": detail}));
    let line = activity::render(&r).unwrap();
    let preview = line.split('`').nth(1).expect("backticked preview");
    assert!(
        preview.chars().count() <= 80,
        "uncapped preview: {}",
        preview.chars().count()
    );
}

#[test]
fn reads_and_searches_show_live_like_any_step() {
    // Hermes' feed: a turn of `cat`/`grep` must not look stalled.
    let read = json!({"phase": "tool_ran", "tool": "read", "detail": "src/lib.rs"});
    let run = json!({"phase": "tool_ran", "tool": "bash", "detail": "cargo test"});
    assert_eq!(
        activity::render(&[read, run]).as_deref(),
        Some("Read `src/lib.rs`\nRunning `cargo test`"),
    );
}

#[test]
fn read_line_names_the_file_and_range() {
    let r = rows(json!({"phase": "tool_ran", "tool": "read", "detail": "config.yaml L110-139"}));
    assert_eq!(
        activity::render(&r).as_deref(),
        Some("Read `config.yaml L110-139`")
    );
}

#[test]
fn write_and_edit_lines_distinguish_themselves() {
    let w = rows(json!({"phase": "tool_ran", "tool": "write", "detail": "src/lib.rs"}));
    assert_eq!(activity::render(&w).as_deref(), Some("Write `src/lib.rs`"));
    let e = rows(json!({"phase": "tool_ran", "tool": "edit", "detail": "src/lib.rs"}));
    assert_eq!(activity::render(&e).as_deref(), Some("Edit `src/lib.rs`"));
}

#[test]
fn other_tools_go_by_their_proper_name() {
    // gray's label wins: the plugin's manifest label, or the humanized id.
    let labeled = rows(json!({
        "phase": "tool_ran", "tool": "discord_send_ui", "label": "Discord Send UI"
    }));
    assert_eq!(
        activity::render(&labeled).as_deref(),
        Some("Discord Send UI")
    );
    // An older gray sends no label: humanize the id the same way.
    let r = rows(json!({"phase": "tool_ran", "tool": "web_search"}));
    assert_eq!(activity::render(&r).as_deref(), Some("Web Search"));
    let p = rows(json!({"phase": "tool_ran", "tool": "discord_send", "detail": "hi there"}));
    assert_eq!(
        activity::render(&p).as_deref(),
        Some("Discord Send `hi there`")
    );
    let single = rows(json!({"phase": "tool_ran", "tool": "custom"}));
    assert_eq!(activity::render(&single).as_deref(), Some("custom"));
    let failed = rows(json!({"phase": "tool_finished", "tool": "web_fetch", "error": true}));
    assert_eq!(
        activity::render(&failed).as_deref(),
        Some("Web Fetch failed")
    );
}

#[test]
fn thinking_is_hidden_but_failures_are_visible() {
    let r = rows(json!({"phase": "thinking", "detail": "the user wants X"}));
    assert_eq!(activity::render(&r), None);
    let f = rows(json!({"phase": "tool_finished", "tool": "bash", "error": true}));
    assert_eq!(activity::render(&f).as_deref(), Some("bash failed"));
    let c = rows(json!({"phase": "compacted"}));
    assert_eq!(activity::render(&c).as_deref(), Some("Context compacted"));
    let w = rows(json!({"phase": "provider_retry", "detail": "Reconnecting... 1/3"}));
    assert_eq!(
        activity::render(&w).as_deref(),
        Some("Provider retry: Reconnecting... 1/3")
    );
}

#[test]
fn quiet_rows_render_to_nothing() {
    assert!(activity::render(&[]).is_none());
    // A successful finish says nothing new: the `ran` line above it did.
    let ok = rows(json!({"phase": "tool_finished", "tool": "bash"}));
    assert!(activity::render(&ok).is_none());
    let started = rows(json!({"phase": "tool_started", "tool": "bash"}));
    assert_eq!(activity::render(&started).as_deref(), Some("Running"));
    let thinking_empty = rows(json!({"phase": "thinking", "detail": ""}));
    assert!(activity::render(&thinking_empty).is_none());
}

#[test]
fn repeated_lines_collapse_and_every_line_is_kept() {
    let batch: Vec<serde_json::Value> = [
        "cargo build",
        "cargo build",
        "git status",
        "cargo clippy",
        "cargo test",
    ]
    .iter()
    .map(|c| json!({"phase": "tool_ran", "tool": "bash", "detail": c}))
    .collect();
    let text = activity::render(&batch).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // Hermes' accumulating bubble: the whole feed, not a sliding window.
    assert_eq!(
        lines.len(),
        4,
        "a repeat collapsed, nothing dropped: {text}"
    );
    assert_eq!(lines[0], "Running `cargo build`");
    assert_eq!(lines[1], "Running `git status`");
    assert_eq!(lines[3], "Running `cargo test`");
}

#[test]
fn the_card_lists_the_view_not_its_ls_neighbor() {
    let batch = vec![
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "gray view /tmp/shot.png"}),
        json!({"phase": "tool_ran", "call_id": "b", "tool": "bash", "detail": "ls -la /home/u"}),
        json!({"phase": "tool_finished", "call_id": "a", "tool": "bash",
               "output": "Image shown: /tmp/shot.png", "elapsed_ms": 300, "turn_ms": 900}),
        json!({"phase": "tool_finished", "call_id": "b", "tool": "bash",
               "output": "total 0", "elapsed_ms": 200, "turn_ms": 900}),
    ];
    let card = activity::render_card(&batch).unwrap();
    assert!(
        card.contains("Ran `gray view /tmp/shot.png` (0.3s)"),
        "{card}"
    );
    assert!(!card.contains("ls -la"), "ls neighbor leaked: {card}");
    assert!(!card.contains("Image shown"), "tool output leaked: {card}");
}

#[test]
fn the_card_lists_actions_and_never_their_output() {
    let batch = vec![
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "cargo test"}),
        json!({"phase": "tool_finished", "call_id": "a", "tool": "bash",
               "output": "line one\nline two", "elapsed_ms": 1200, "turn_ms": 3400}),
    ];
    let card = activity::render_card(&batch).unwrap();
    assert!(card.starts_with("⋯ 3.4s · ran 1 command"), "{card}");
    assert!(card.contains("Ran `cargo test` (1.2s)"), "{card}");
    assert!(!card.contains("line one"), "tool output leaked: {card}");
    assert!(!card.contains("```"), "output fenced into the card: {card}");
}

#[test]
fn the_card_pairs_parallel_calls_by_call_id() {
    let batch = vec![
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "first"}),
        json!({"phase": "tool_ran", "call_id": "b", "tool": "bash", "detail": "second"}),
        json!({"phase": "tool_finished", "call_id": "b", "tool": "bash",
               "output": "B", "elapsed_ms": 900, "turn_ms": 2500}),
        json!({"phase": "tool_finished", "call_id": "a", "tool": "bash",
               "output": "A", "elapsed_ms": 400, "turn_ms": 2000}),
    ];
    let card = activity::render_card(&batch).unwrap();
    assert!(card.starts_with("⋯ 2.5s · ran 2 commands"), "{card}");
    assert!(card.contains("Ran `first` (0.4s)"), "{card}");
    assert!(card.contains("Ran `second` (0.9s)"), "{card}");
    assert!(!card.contains("```"), "output fenced into the card: {card}");
}

// ── the gate ──

#[test]
fn narration_is_on_unless_explicitly_off() {
    assert!(activity::enabled(&json!({})));
    assert!(activity::enabled(&json!({"activity_indicator": true})));
    assert!(!activity::enabled(&json!({"activity_indicator": false})));
    // A junk value is not a decision: stay on.
    assert!(activity::enabled(
        &json!({"activity_indicator": "no thanks"})
    ));
}

// ── the sink ──

#[test]
fn the_sink_is_bounded_and_drains() {
    let s = activity::sink();
    let total = activity::MAX_PENDING + 200;
    for i in 0..total {
        activity::push(&s, json!({"phase": "tool_ran", "detail": i.to_string()}));
    }
    let got = activity::drain(&s);
    assert_eq!(got.len(), activity::MAX_PENDING, "bounded queue");
    // The newest survive: that is the action in flight.
    assert_eq!(got.last().unwrap()["detail"], (total - 1).to_string());
    assert!(activity::drain(&s).is_empty(), "drain empties the queue");
    assert!(activity::finish(&s, "").len() <= 128);
    assert!(activity::finish(&s, "").is_empty(), "finish is one-shot");
}

#[test]
fn the_sink_stamps_durations_and_the_turn_clock() {
    let s = activity::sink();
    activity::begin(&s, "");
    activity::push(
        &s,
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "ls"}),
    );
    activity::push(
        &s,
        json!({"phase": "tool_finished", "call_id": "a", "tool": "bash", "output": "secret output"}),
    );
    let rows = activity::finish(&s, "");
    assert_eq!(rows.len(), 2);
    assert!(
        rows[1]["elapsed_ms"].is_u64(),
        "no call duration: {}",
        rows[1]
    );
    assert!(rows[1]["turn_ms"].is_u64(), "no turn clock: {}", rows[1]);
    let card = activity::render_card(&rows).unwrap();
    assert!(card.contains("ran 1 command"), "{card}");
    assert!(
        !card.contains("secret output"),
        "tool output leaked: {card}"
    );
}

#[test]
fn scoped_sinks_do_not_mix_conversations() {
    let s = activity::sink();
    activity::begin(&s, "one");
    activity::begin(&s, "two");
    activity::push_for(
        &s,
        "one",
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "one"}),
    );
    activity::push_for(
        &s,
        "two",
        json!({"phase": "tool_ran", "call_id": "b", "tool": "bash", "detail": "two"}),
    );
    let one = activity::drain_for(&s, "one");
    let two = activity::drain_for(&s, "two");
    assert_eq!(one[0]["detail"], "one");
    assert_eq!(two[0]["detail"], "two");
    assert_eq!(activity::finish(&s, "one").len(), 1);
    assert_eq!(activity::finish(&s, "two").len(), 1);

    activity::begin(&s, "one");
    activity::push_for(&s, "one", json!({"phase": "tool_ran", "detail": "stale"}));
    activity::begin(&s, "one");
    activity::push_for(&s, "one", json!({"phase": "tool_ran", "detail": "fresh"}));
    let fresh = activity::finish(&s, "one");
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0]["detail"], "fresh");
}

// ── wiring: one live bubble per channel/conversation, plus a final card ──

/// A runtime wired for narration. A macro, not a fn: the runner/deliver
/// closure types would otherwise have to be spelled out by hand.
macro_rules! narrated {
    ($config:expr, $sink:expr, $seen:expr, $clock:expr) => {{
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
        let _keep = Box::leak(Box::new(tmp)); // the store outlives the temp dir otherwise
        let runner = move |_c: &serde_json::Value,
                           _p: &std::path::Path,
                           _conv: &str,
                           _prompt: &str| {
            Box::pin(async move { Ok::<String, gray_discord::runner::RunError>("answer".into()) })
        };
        let deliver = move |_p: gray_discord::durable::OutboxPart| {
            Box::pin(async move { Ok::<String, String>("sent".into()) })
        };
        let t = $clock.clone();
        let seen = $seen.clone();
        Runtime::new($config, path, store, deliver, runner)
            .with_activity($sink.clone())
            .with_clock(std::sync::Arc::new(move || *t.lock().unwrap()))
            .with_activity_hook(std::sync::Arc::new(move |text: &str, edit: bool| {
                seen.lock().unwrap().push((text.to_string(), edit));
            }))
    }};
}

#[tokio::test]
async fn the_second_line_edits_the_first_bubble() {
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({}), sink, seen, clock);

    activity::push(
        &sink,
        json!({"phase": "tool_ran", "tool": "bash", "detail": "ls"}),
    );
    rt.report_activity("42").await;
    activity::push(
        &sink,
        json!({"phase": "tool_ran", "tool": "bash", "detail": "cargo test"}),
    );
    *clock.lock().unwrap() = 5.0;
    rt.report_activity("42").await;

    let log = seen.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0], ("-# Running `ls`".to_string(), false), "first post");
    assert_eq!(
        log[1],
        ("-# Running `ls`\n-# Running `cargo test`".to_string(), true),
        "then an edit"
    );
}

#[tokio::test]
async fn gated_rows_are_kept_until_the_gap_passes() {
    // The stuck-bubble repro: `tool_started` posts a bare "Running" line and
    // the `tool_ran` with the command arrives inside the edit gap. Dropping
    // the gated rows sticks the bubble bare until the turn ends; keeping
    // them updates it on the next tick.
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({}), sink, seen, clock);

    activity::push(
        &sink,
        json!({"phase": "tool_started", "call_id": "a", "tool": "bash"}),
    );
    rt.report_activity("42").await;
    activity::push(
        &sink,
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "cargo test"}),
    );
    rt.report_activity("42").await;
    *clock.lock().unwrap() = 5.0;
    rt.report_activity("42").await;

    let log = seen.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0], ("-# Running".to_string(), false), "first post");
    assert_eq!(
        log[1],
        ("-# Running `cargo test`".to_string(), true),
        "the command must follow, not stick bare"
    );
}

#[tokio::test]
async fn final_flush_settles_the_card_and_next_turn_gets_a_new_one() {
    // No separate tally card: the turn's own card settles (green, done
    // footer) and stays as the record.
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({}), sink, seen, clock);

    rt.begin_activity("42", "chat:one").await;
    activity::push_for(
        &sink,
        "chat:one",
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "cargo test"}),
    );
    rt.report_activity_for("42", "chat:one", false).await;
    rt.report_activity_for("42", "chat:one", true).await;

    rt.begin_activity("42", "chat:one").await;
    activity::push_for(
        &sink,
        "chat:one",
        json!({"phase": "tool_ran", "call_id": "b", "tool": "bash", "detail": "pwd"}),
    );
    rt.report_activity_for("42", "chat:one", false).await;

    let log = seen.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            ("-# Running `cargo test`".to_string(), false),
            ("-# Running `cargo test`".to_string(), true),
            ("-# Running `pwd`".to_string(), false),
        ],
        "post, settle in place, then a fresh card for the next turn"
    );
}

#[tokio::test]
async fn the_opt_in_card_still_follows_the_bubble() {
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({"activity_card": true}), sink, seen, clock);

    rt.begin_activity("42", "chat:one").await;
    activity::push_for(
        &sink,
        "chat:one",
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "ls"}),
    );
    activity::push_for(
        &sink,
        "chat:one",
        json!({"phase": "tool_finished", "call_id": "a", "tool": "bash", "output": "one\ntwo"}),
    );
    rt.report_activity_for("42", "chat:one", false).await;
    rt.report_activity_for("42", "chat:one", true).await;

    rt.begin_activity("42", "chat:one").await;
    activity::push_for(
        &sink,
        "chat:one",
        json!({"phase": "tool_ran", "call_id": "b", "tool": "bash", "detail": "pwd"}),
    );
    rt.report_activity_for("42", "chat:one", false).await;

    let log = seen.lock().unwrap().clone();
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(log[0].0.starts_with("-# Ran `ls`"), "first post: {log:?}");
    assert!(log[1].1, "the turn's card settles in place: {log:?}");
    assert!(log[2].0.starts_with("⋯ "), "tally card: {log:?}");
    assert!(
        !log[2].0.contains("one\ntwo"),
        "tool output leaked: {log:?}"
    );
    assert_eq!(
        log[3],
        ("-# Running `pwd`".to_string(), false),
        "new turn card"
    );
}

#[tokio::test]
async fn final_card_reaches_the_discord_rest_endpoint() {
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let stub = common::Stub::start().await;
    let rt = narrated!(json!({"activity_card": true}), sink, seen, clock)
        .with_rest(Rest::new(&stub.base, "TESTTOKEN"));

    rt.begin_activity("42", "chat:rest").await;
    activity::push_for(
        &sink,
        "chat:rest",
        json!({"phase": "tool_ran", "call_id": "a", "tool": "bash", "detail": "printf hi"}),
    );
    activity::push_for(
        &sink,
        "chat:rest",
        json!({"phase": "tool_finished", "call_id": "a", "tool": "bash", "output": "hi"}),
    );
    rt.report_activity_for("42", "chat:rest", false).await;
    rt.report_activity_for("42", "chat:rest", true).await;

    let sent: Vec<common::Sent> = stub
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sent| sent.method == "POST")
        .cloned()
        .collect();
    assert_eq!(sent.len(), 2, "expected the turn card plus the tally card");
    let turn = sent[0].body["components"].as_array().unwrap();
    assert_eq!(turn[0]["type"], 10, "the tool lines as V2 text");
    assert_eq!(
        turn.last().unwrap()["type"],
        17,
        "the status chip is a V2 Container"
    );
    let card = component_text(&sent[1].body["components"]);
    assert!(card.contains("gray · done"), "{card}");
    assert!(card.contains("Ran `printf hi`"), "{card}");
    assert!(!card.contains("```"), "output fenced into the card: {card}");
    assert_eq!(sent[1].body["flags"], json!(32768));
    assert!(sent[1].body.get("content").is_none());
    assert!(sent[1].body.get("embeds").is_none());
}

#[tokio::test]
async fn off_activity_narrates_nothing() {
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({"activity_indicator": false}), sink, seen, clock);

    activity::push(
        &sink,
        json!({"phase": "tool_ran", "tool": "bash", "detail": "rm -rf /"}),
    );
    rt.report_activity("42").await;
    assert!(seen.lock().unwrap().is_empty());
    // And the queue does not grow behind the off switch.
    assert!(activity::drain(&sink).is_empty());
}

#[tokio::test]
async fn an_unchanged_card_is_not_reposted() {
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({}), sink, seen, clock);

    activity::push(
        &sink,
        json!({"phase": "tool_ran", "tool": "bash", "detail": "ls"}),
    );
    for _ in 0..3 {
        // Past the edit gap, short of the footer clock's next step.
        *clock.lock().unwrap() += 2.0;
        rt.report_activity("42").await;
    }
    assert_eq!(seen.lock().unwrap().len(), 1, "a quiet turn sends nothing");
}

// ── the wire: real gray rows, through the real runner, into the bubble ──

fn write_exe(path: &std::path::Path, script: &str) {
    std::fs::write(path, script).expect("fixture write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// The exact rows gray's `--json` emitter produces for one turn: a tool
/// call narrated, its completed output, a thinking batch, then the answer.
/// If gray changes the shape, this fails here rather than silently going
/// quiet on Discord.
const GRAY_TURN: &str = r#"#!/bin/sh
cat <<'JSON'
{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"progress","phase":"tool_started","call_id":"c1","tool":"bash"}
{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"progress","phase":"tool_ran","call_id":"c1","tool":"bash","detail":"cargo test -p gray"}
{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"progress","phase":"tool_finished","call_id":"c1","tool":"bash","output":"test result: ok\n0 passed"}
{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"progress","phase":"thinking","detail":"run the tests first"}
{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"result","text":"done","usage":{}}
JSON
"#;

#[tokio::test]
async fn a_real_gray_turn_narrates_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("gray-home");
    std::fs::create_dir(&home).unwrap();
    gray_discord::config::atomic_json(
        &home.join("config.json"),
        &serde_json::json!({"model": "test"}),
    )
    .unwrap();
    let exe = root.join("gray");
    write_exe(&exe, GRAY_TURN);
    let config = json!({
        "gray_bin": exe.to_str().unwrap(),
        "gray_home": home.to_str().unwrap()
    });

    let sink = activity::sink();
    let opts = gray_discord::runner::RunOpts {
        progress: Some(activity::callback(sink.clone())),
        ..gray_discord::runner::default_opts()
    };
    let answer = gray_discord::runner::run_gray(
        &config,
        &root.join("plugin.json"),
        "chat:one",
        "hello",
        opts,
    )
    .await
    .unwrap();
    assert_eq!(answer, "done");

    let rows = activity::drain(&sink);
    assert_eq!(rows.len(), 4, "one row per progress event: {rows:?}");
    let bubble = activity::render(&rows).unwrap();
    assert!(bubble.starts_with("Ran `cargo test -p gray`"), "{bubble}");
    assert!(bubble.ends_with(')'), "duration missing: {bubble}");
    let card = activity::render_card(&activity::finish(&sink, "")).unwrap();
    assert!(card.contains("cargo test -p gray"), "{card}");
    assert!(
        !card.contains("test result: ok"),
        "tool output leaked: {card}"
    );
    assert!(
        !card.contains("run the tests first"),
        "reasoning leaked: {card}"
    );
}

#[test]
fn a_shell_call_without_a_command_names_the_background_job() {
    let ran = rows(json!({"phase": "tool_ran", "call_id": "a", "tool": "bash"}));
    assert_eq!(
        activity::render(&ran).as_deref(),
        Some("Checking background job")
    );
    let mut done = ran.clone();
    done.push(json!({"phase": "tool_finished", "call_id": "a", "tool": "bash", "elapsed_ms": 20}));
    assert_eq!(
        activity::render(&done).as_deref(),
        Some("Checked background job (0.0s)"),
        "never a bare `Ran (0.0s)`"
    );
}
