//! Narration: gray's `--json` progress rows -> one status bubble.

use gray_discord::activity;
use gray_discord::durable::Store;
use gray_discord::gateway::Runtime;
use serde_json::json;

// ── rendering (Hermes parity) ──

fn rows(v: serde_json::Value) -> Vec<serde_json::Value> {
    vec![v]
}

#[test]
fn bash_line_matches_hermes_format() {
    let r = rows(json!({"phase": "tool_ran", "tool": "bash", "detail": "ls -la"}));
    assert_eq!(activity::render(&r).as_deref(), Some("💻 terminal: ls -la"));
}

#[test]
fn read_line_names_the_file_and_range() {
    let r = rows(json!({"phase": "tool_ran", "tool": "read", "detail": "config.yaml L110-139"}));
    assert_eq!(
        activity::render(&r).as_deref(),
        Some("📖 Reading config.yaml L110-139")
    );
}

#[test]
fn write_and_edit_lines_distinguish_themselves() {
    let w = rows(json!({"phase": "tool_ran", "tool": "write", "detail": "src/lib.rs"}));
    assert_eq!(
        activity::render(&w).as_deref(),
        Some("✍️ Writing src/lib.rs")
    );
    let e = rows(json!({"phase": "tool_ran", "tool": "edit", "detail": "src/lib.rs"}));
    assert_eq!(
        activity::render(&e).as_deref(),
        Some("✍️ Editing src/lib.rs")
    );
}

#[test]
fn unknown_tool_still_names_itself() {
    let r = rows(json!({"phase": "tool_ran", "tool": "web_search"}));
    assert_eq!(activity::render(&r).as_deref(), Some("🔧 web_search"));
    let p = rows(json!({"phase": "tool_ran", "tool": "gray_plugin", "detail": "do thing"}));
    assert_eq!(
        activity::render(&p).as_deref(),
        Some("🔧 gray_plugin: do thing")
    );
}

#[test]
fn thinking_and_failures_are_visible() {
    let r = rows(json!({"phase": "thinking", "detail": "the user wants X"}));
    assert_eq!(activity::render(&r).as_deref(), Some("🧠 the user wants X"));
    let f = rows(json!({"phase": "tool_finished", "tool": "bash", "error": true}));
    assert_eq!(activity::render(&f).as_deref(), Some("❌ bash failed"));
    let c = rows(json!({"phase": "compacted"}));
    assert_eq!(activity::render(&c).as_deref(), Some("🗜 context compacted"));
    let w = rows(json!({"phase": "provider_retry", "detail": "Reconnecting... 1/3"}));
    assert_eq!(
        activity::render(&w).as_deref(),
        Some("⚠️ Reconnecting... 1/3")
    );
}

#[test]
fn quiet_rows_render_to_nothing() {
    assert!(activity::render(&[]).is_none());
    // A successful finish says nothing new: the `ran` line above it did.
    let ok = rows(json!({"phase": "tool_finished", "tool": "bash"}));
    assert!(activity::render(&ok).is_none());
    let started = rows(json!({"phase": "tool_started", "tool": "bash"}));
    assert!(activity::render(&started).is_none());
    let thinking_empty = rows(json!({"phase": "thinking", "detail": ""}));
    assert!(activity::render(&thinking_empty).is_none());
}

#[test]
fn repeated_lines_collapse_and_the_tail_is_kept() {
    let batch: Vec<serde_json::Value> = ["ls", "ls", "cat x", "grep y", "cargo test"]
        .iter()
        .map(|c| json!({"phase": "tool_ran", "tool": "bash", "detail": c}))
        .collect();
    let text = activity::render(&batch).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "keeps the last three: {text}");
    assert_eq!(lines[2], "💻 terminal: cargo test");
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
    for i in 0..200 {
        activity::push(&s, json!({"phase": "tool_ran", "detail": i.to_string()}));
    }
    let got = activity::drain(&s);
    assert!(got.len() <= 64, "unbounded queue: {}", got.len());
    // The newest survive: that is the action in flight.
    assert_eq!(got.last().unwrap()["detail"], "199");
    assert!(activity::drain(&s).is_empty(), "drain empties the queue");
}

#[test]
fn publishing_needs_new_content_and_a_gap() {
    assert!(activity::should_publish("a", None, 0.0, -1.0));
    assert!(
        !activity::should_publish("a", Some("a"), 99.0, 0.0),
        "no-op edit"
    );
    assert!(
        !activity::should_publish("b", Some("a"), 0.5, 0.0),
        "inside the gap"
    );
    assert!(activity::should_publish("b", Some("a"), 1.5, 0.0));
}

// ── wiring: one bubble per channel, edited in place ──

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
    assert_eq!(log[0], ("💻 terminal: ls".to_string(), false), "first post");
    assert_eq!(
        log[1],
        ("💻 terminal: cargo test".to_string(), true),
        "then an edit"
    );
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
async fn an_unchanged_bubble_is_not_reposted() {
    let sink = activity::sink();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let clock = std::sync::Arc::new(std::sync::Mutex::new(0.0));
    let rt = narrated!(json!({}), sink, seen, clock);

    for _ in 0..3 {
        activity::push(
            &sink,
            json!({"phase": "tool_ran", "tool": "bash", "detail": "ls"}),
        );
        *clock.lock().unwrap() += 10.0;
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
/// call narrated, a thinking batch, then the answer. If gray changes the
/// shape, this fails here rather than silently going quiet on Discord.
const GRAY_TURN: &str = r#"#!/bin/sh
cat <<'JSON'
{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"progress","phase":"tool_ran","tool":"bash","detail":"cargo test -p gray"}
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
    assert_eq!(rows.len(), 2, "one row per narrated action: {rows:?}");
    let bubble = activity::render(&rows).unwrap();
    assert_eq!(
        bubble,
        "💻 terminal: cargo test -p gray\n🧠 run the tests first"
    );
}
