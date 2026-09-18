use gray_discord::durable::Store;

#[tokio::test]
async fn failed_delivery_restart_does_not_repeat_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let calls_runner = calls.clone();
    let runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, prompt: &str| {
            let calls = calls_runner.clone();
            let prompt = prompt.to_string();
            Box::pin(async move {
                calls.lock().unwrap().push(prompt);
                Ok("answer".to_string())
            })
        };

    let broken = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Err("private error body".to_string()) })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({}),
        path.clone(),
        store.clone(),
        broken,
        runner.clone(),
    );

    store
        .enqueue("message1", "42", "question", None, 1000)
        .unwrap();
    assert!(runtime.generate_one().await.unwrap());
    assert!(runtime.deliver_one().await.unwrap());

    assert_eq!(*calls.lock().unwrap(), vec!["question"]);

    let sent_deliver = sent.clone();
    let deliver = move |part: gray_discord::durable::OutboxPart| {
        let sent = sent_deliver.clone();
        Box::pin(async move {
            sent.lock().unwrap().push(part.content);
            Ok("reply1".to_string())
        })
    };

    let runtime2 = gray_discord::gateway::Runtime::new(
        serde_json::json!({}),
        path,
        Store::new(store.path()).unwrap(),
        deliver,
        runner,
    );

    runtime2.store.recover().unwrap();
    {
        let conn = rusqlite::Connection::open(store.path()).unwrap();
        conn.execute("UPDATE outbox SET next_at=0", []).unwrap();
    }

    assert!(!runtime2.generate_one().await.unwrap());
    assert!(runtime2.deliver_one().await.unwrap());

    assert_eq!(*calls.lock().unwrap(), vec!["question"]);
    assert_eq!(*sent.lock().unwrap(), vec!["answer"]);
    assert_eq!(store.get("message1").unwrap().unwrap().state, "sent");
}

#[tokio::test]
async fn cancel_running_work_and_shutdown_marks_uncertain() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(started_tx)));

    let runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            let tx = started_tx.clone();
            Box::pin(async move {
                if let Some(t) = tx.lock().unwrap().take() {
                    let _ = t.send(());
                }
                std::future::pending::<Result<String, gray_discord::runner::RunError>>().await
            })
        };

    let deliver = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Ok("reply".to_string()) })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({}),
        path,
        store.clone(),
        deliver,
        runner,
    );

    store.enqueue("1", "42", "question", None, 1000).unwrap();
    let gen_task = tokio::spawn(async move { runtime.generate_one().await });

    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("started")
        .expect("channel");

    store.cancel("1").unwrap();

    let res = tokio::time::timeout(std::time::Duration::from_secs(3), gen_task)
        .await
        .expect("not timed out")
        .expect("task join");

    assert!(res.unwrap());
    assert_eq!(
        store.get("1").unwrap().unwrap().error.as_deref(),
        Some("cancelled")
    );
    assert!(store.claim().unwrap().is_none());
}

#[tokio::test]
async fn slash_followup_routing_marks_part_as_interaction() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();

    let runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            Box::pin(async move { Ok("slash answer".to_string()) })
        };

    let delivered_parts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let delivered_parts_clone = delivered_parts.clone();
    let deliver = move |part: gray_discord::durable::OutboxPart| {
        let parts = delivered_parts_clone.clone();
        Box::pin(async move {
            parts.lock().unwrap().push(part);
            Ok("slash-reply-id".to_string())
        })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({}),
        path,
        store.clone(),
        deliver,
        runner,
    );

    store
        .enqueue("slash1", "42", "ask gray something", None, 1000)
        .unwrap();
    store
        .set_interaction("slash1", "interaction_token_abc", "app_id_xyz")
        .unwrap();

    assert!(runtime.generate_one().await.unwrap());
    assert!(runtime.deliver_one().await.unwrap());

    let recorded = delivered_parts.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].id, "slash1");
    assert_eq!(recorded[0].content, "slash answer");
    assert_eq!(
        recorded[0].interaction_token.as_deref(),
        Some("interaction_token_abc")
    );
    assert_eq!(recorded[0].app_id.as_deref(), Some("app_id_xyz"));
    assert_eq!(store.get("slash1").unwrap().unwrap().state, "sent");
}
