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
    store.schedule_add("job", 60, "check", 0.0).unwrap();
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
