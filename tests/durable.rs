use gray_discord::durable::Store;

fn store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("queue.sqlite");
    let s = Store::new(&path).unwrap();
    (tmp, s)
}

#[test]
fn deduplication_serial_claim_and_restart() {
    let (_tmp, store) = store();
    assert!(store.enqueue("1", "42", "one", None, 1000).unwrap());
    assert!(!store.enqueue("1", "42", "duplicate", None, 1000).unwrap());
    store.enqueue("2", "42", "two", None, 1000).unwrap();
    store.enqueue("3", "43", "three", None, 1000).unwrap();
    let first = store.claim().unwrap().expect("claim 1");
    assert_eq!(first.id, "1");
    assert_eq!(store.claim().unwrap().expect("claim 3").id, "3");
    assert!(store.claim().unwrap().is_none());
    let path = store.path().to_path_buf();
    drop(store);
    let restarted = Store::new(&path).unwrap();
    restarted.recover().unwrap();
    assert_eq!(restarted.get("1").unwrap().expect("row").state, "uncertain");
    assert_eq!(restarted.claim().unwrap().expect("claim 2").id, "2");
}

#[test]
fn delivery_retry_never_reruns_generation() {
    let (_tmp, store) = store();
    store.enqueue("1", "42", "one", None, 1000).unwrap();
    let item = store.claim().unwrap().expect("claim");
    store
        .complete(&item.id, &"a".repeat(2100), &serde_json::json!({}))
        .unwrap();
    assert!(store.claim().unwrap().is_none());
    let part = store.next_delivery(0.0).unwrap().expect("part 0");
    store.ack(&part.id, part.part, "discord-message-1").unwrap();
    let remaining = store.next_delivery(0.0).unwrap().expect("part 1");
    assert_eq!(remaining.part, 1);
    store.delivery_failed(&remaining, "network", 0.0).unwrap();
    assert!(store.next_delivery(0.0).unwrap().is_none());
    let path = store.path().to_path_buf();
    drop(store);
    let restarted = Store::new(&path).unwrap();
    restarted.recover().unwrap();
    assert!(restarted.claim().unwrap().is_none());
    let part = restarted.next_delivery(1000.0).unwrap().expect("retry");
    assert_eq!(part.part, 1);
    restarted
        .ack(&part.id, part.part, "discord-message-2")
        .unwrap();
    assert_eq!(restarted.get("1").unwrap().expect("row").state, "sent");
}

#[test]
fn online_schedule_and_atomic_due_enqueue() {
    let (_tmp, store) = store();
    let other = Store::new(store.path()).unwrap();
    store
        .schedule_add("job", 60, "check", "42", "chat:42", 0.0)
        .unwrap();
    assert_eq!(other.schedules().unwrap()[0].id, "job");
    other.enqueue_due("42", 61.0).unwrap();
    store.enqueue_due("42", 61.0).unwrap();
    assert!(store.claim().unwrap().is_some());
    assert!(store.claim().unwrap().is_none());
    other.schedule_remove("job").unwrap();
    assert!(other.schedules().unwrap().is_empty());
}

#[test]
fn cancel_and_queue_capacity() {
    let (_tmp, store) = store();
    store.enqueue("1", "42", "one", None, 1).unwrap();
    assert!(store.enqueue("2", "42", "two", None, 1).is_err());
    store.cancel("1").unwrap();
    assert!(store.claim().unwrap().is_none());
    assert_eq!(store.get("1").unwrap().expect("row").state, "cancelled");
}

#[test]
fn reset_cancels_queued_and_running_conversation_work() {
    let (_tmp, store) = store();
    store
        .enqueue("running", "42", "one", Some("chat:42:user:7"), 1000)
        .unwrap();
    let first = store.claim().unwrap().expect("running row");
    store
        .enqueue("queued", "42", "two", Some("chat:42:user:7"), 1000)
        .unwrap();
    assert!(store.cancel_pending_conversation("chat:42:user:7").unwrap());
    assert!(store.get(&first.id).unwrap().expect("running").cancel);
    assert_eq!(
        store.get("queued").unwrap().expect("queued").state,
        "cancelled"
    );
    assert!(store.claim().unwrap().is_none());
}

#[test]
fn prune_removes_old_terminal_rows_only() {
    let (_tmp, store) = store();
    store.enqueue("old", "42", "gone", None, 1000).unwrap();
    let item = store.claim().unwrap().expect("claim").id;
    store
        .complete(&item, "done", &serde_json::json!({}))
        .unwrap();
    let part = store.next_delivery(0.0).unwrap().expect("part");
    store.ack(&part.id, part.part, "m1").unwrap();
    assert_eq!(store.get("old").unwrap().expect("row").state, "sent");
    store.enqueue("fresh", "42", "stay", None, 1000).unwrap();
    // Backdate only the terminal row far into the past.
    {
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        conn.execute("UPDATE inbox SET created = 0 WHERE id = 'old'", [])
            .unwrap();
    }
    let removed = store.prune_terminal(7 * 86400, 8.0 * 86400.0).unwrap();
    assert_eq!(removed, 1);
    assert!(store.get("old").unwrap().is_none());
    assert!(store.get("fresh").unwrap().is_some());
}

#[test]
fn a_schedule_delivers_back_to_the_channel_it_was_created_in() {
    let (_tmp, store) = store();
    store
        .schedule_add("j", 60, "check the thing", "777", "chat:777", 0.0)
        .unwrap();
    let job = &store.schedules().unwrap()[0];
    assert_eq!(job.channel, "777");
    assert_eq!(job.conversation, "chat:777");
    // The daemon ticks with the configured home channel. A job created in
    // another channel must not be dragged over to it.
    store.enqueue_due("home", 61.0).unwrap();
    let row = store.get("job:j:60").unwrap().expect("enqueued");
    assert_eq!(row.channel, "777");
    assert_eq!(row.conversation, "chat:777");
}

#[test]
fn a_pre_conversation_schedule_keeps_the_configured_home_channel() {
    // A row written by the binary before schedules carried a target: the
    // migration fills '' and the ticker must fall back to the home channel,
    // not enqueue into a blank one.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("queue.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schedules(id TEXT PRIMARY KEY, interval INTEGER NOT NULL,
                 prompt TEXT NOT NULL, next_at REAL NOT NULL,
                 status TEXT NOT NULL DEFAULT 'scheduled');
             CREATE TABLE inbox(id TEXT PRIMARY KEY, channel TEXT NOT NULL,
                 conversation TEXT NOT NULL, prompt TEXT NOT NULL,
                 state TEXT NOT NULL DEFAULT 'queued', created REAL NOT NULL,
                 error TEXT, receipt TEXT, cancel INTEGER DEFAULT 0,
                 interaction_token TEXT, app_id TEXT);
             INSERT INTO schedules(id,interval,prompt,next_at,status)
                 VALUES('old',60,'legacy',60,'scheduled');",
        )
        .unwrap();
    }
    let store = Store::new(&path).unwrap();
    assert_eq!(store.schedules().unwrap().len(), 1);
    store.enqueue_due("legacy-home", 61.0).unwrap();
    let row = store.get("job:old:60").unwrap().expect("legacy enqueued");
    assert_eq!(row.channel, "legacy-home");
    assert_eq!(row.conversation, "job:old");
}
