//! Reconnect backfill: cursor persistence, admission, dedupe, caps.
mod common;

use gray_discord::backfill;
use gray_discord::durable::Store;
use gray_discord::transport::Rest;
use serde_json::{json, Value};

const OWNER: &str = "42";
const STRANGER: &str = "77";
const BOT: &str = "123";

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(&dir.path().join("queue.sqlite")).unwrap();
    (dir, store)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Snowflake dated `offset_ms` ago — admission's age filter reads the id.
fn flake(offset_ms: i64) -> u64 {
    ((now_ms() - offset_ms - 1_420_070_400_000).max(0) as u64) << 22
}

fn dm(id: u64, author: &str, content: &str) -> Value {
    json!({
        "id": id.to_string(),
        "channel_id": "9",
        "content": content,
        "author": {"id": author},
    })
}

fn inbox_prompt(store: &Store, id: u64) -> Option<String> {
    store.get(&id.to_string()).ok().flatten().map(|i| i.prompt)
}

#[test]
fn cursor_roundtrip_and_ignores_garbage() {
    let (_dir, store) = store();
    backfill::note_message(&store, "9", "111");
    backfill::note_message(&store, "10", "222");
    backfill::note_message(&store, "", "333"); // dropped: no channel
    store.meta_set("last_msg:bad", "not-a-number").unwrap();
    let mut got = backfill::cursors(&store);
    got.sort();
    assert_eq!(
        got,
        vec![("10".to_string(), 222u64), ("9".to_string(), 111u64)]
    );
}

#[test]
fn inbox_seeds_cursor_for_channels_never_seen_live() {
    let (_dir, store) = store();
    store.enqueue("500", "9", "old", Some("chat:9"), 1).unwrap();
    store
        .enqueue("700", "9", "newer", Some("chat:9"), 2)
        .unwrap();
    store
        .enqueue("600", "10", "other", Some("chat:10"), 3)
        .unwrap();
    backfill::note_message(&store, "10", "650"); // stamped cursor wins
    let mut got = backfill::cursors(&store);
    got.sort();
    assert_eq!(
        got,
        vec![("10".to_string(), 650u64), ("9".to_string(), 700u64)]
    );
}

#[tokio::test]
async fn enqueues_missed_dm_and_advances_cursor() {
    let (_dir, store) = store();
    let cursor = flake(120_000); // two minutes ago
    backfill::note_message(&store, "9", &cursor.to_string());

    let missed = flake(90_000);
    let stranger_msg = flake(60_000);
    // Stub serves Discord's newest-first wire order.
    let stub = common::Stub::start().await;
    *stub.backfill_messages.lock().unwrap() = json!({
        "9": [dm(stranger_msg, STRANGER, "stranger"), dm(missed, OWNER, "missed you")]
    });
    let rest = Rest::new(&stub.base, "TESTTOKEN");

    let accepted = backfill::run(&rest, &store, OWNER, &[], BOT, 100).await;
    assert_eq!(accepted, 1);
    assert_eq!(inbox_prompt(&store, missed), Some("missed you".to_string()));
    assert_eq!(inbox_prompt(&store, stranger_msg), None);

    // The cursor passes over the skipped stranger message too — it is never
    // reconsidered on the next boot.
    let cur = backfill::cursors(&store)
        .into_iter()
        .find(|(ch, _)| ch == "9")
        .unwrap();
    assert_eq!(cur.1, stranger_msg);
}

#[tokio::test]
async fn dedupe_and_age_cap() {
    let (_dir, store) = store();
    let cursor = flake(120_000);
    backfill::note_message(&store, "9", &cursor.to_string());

    // Already delivered live: same id sits in the inbox → Ok(false) path.
    let dup = flake(80_000);
    store
        .enqueue(&dup.to_string(), "9", "already", Some("chat:9"), 100)
        .unwrap();
    // Older than MAX_AGE_MINUTES: skipped, cursor still advances.
    let ancient = flake(backfill::MAX_AGE_MINUTES * 60_000 + 60_000);
    let fresh = flake(70_000);

    let stub = common::Stub::start().await;
    *stub.backfill_messages.lock().unwrap() = json!({
        "9": [dm(fresh, OWNER, "fresh"), dm(dup, OWNER, "dup"), dm(ancient, OWNER, "ancient")]
    });
    let rest = Rest::new(&stub.base, "TESTTOKEN");

    let accepted = backfill::run(&rest, &store, OWNER, &[], BOT, 100).await;
    assert_eq!(accepted, 1, "only the unseen recent message is queued");
    assert_eq!(inbox_prompt(&store, fresh), Some("fresh".to_string()));
    assert_eq!(inbox_prompt(&store, ancient), None);
    assert_eq!(
        backfill::cursors(&store)
            .into_iter()
            .find(|(ch, _)| ch == "9")
            .unwrap()
            .1,
        fresh
    );
}

#[tokio::test]
async fn no_cursor_no_replay() {
    let (_dir, store) = store();
    let stub = common::Stub::start().await;
    *stub.backfill_messages.lock().unwrap() = json!({
        "9": [dm(flake(10_000), OWNER, "hi")]
    });
    let rest = Rest::new(&stub.base, "TESTTOKEN");

    // First boot on a fresh install: history is not ours to replay.
    assert_eq!(backfill::run(&rest, &store, OWNER, &[], BOT, 100).await, 0);
    assert_eq!(store.pending_count().unwrap(), 0);
}

#[tokio::test]
async fn guild_message_needs_mention() {
    let (_dir, store) = store();
    let cursor = flake(120_000);
    backfill::note_message(&store, "9", &cursor.to_string());

    let no_mention = flake(90_000);
    let mention = flake(80_000);
    let stub = common::Stub::start().await;
    *stub.backfill_messages.lock().unwrap() = json!({
        "9": [
            json!({"id": mention.to_string(), "channel_id": "9", "guild_id": "5",
                   "content": format!("hey <@{BOT}>"), "author": {"id": OWNER}}),
            json!({"id": no_mention.to_string(), "channel_id": "9", "guild_id": "5",
                   "content": "no mention", "author": {"id": OWNER}}),
        ]
    });
    let rest = Rest::new(&stub.base, "TESTTOKEN");

    let accepted = backfill::run(&rest, &store, OWNER, &[], BOT, 100).await;
    assert_eq!(accepted, 1);
    assert_eq!(inbox_prompt(&store, mention), Some("hey".to_string()));
    assert_eq!(inbox_prompt(&store, no_mention), None);
}
