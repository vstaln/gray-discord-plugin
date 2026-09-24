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

#[tokio::test]
async fn reaction_call_sequence_on_success_and_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();

    let reactions_log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let reactions_log_clone = reactions_log.clone();
    let hook = std::sync::Arc::new(move |op: &str, msg_id: &str, emoji: &str| {
        reactions_log_clone.lock().unwrap().push((
            op.to_string(),
            msg_id.to_string(),
            emoji.to_string(),
        ));
    });

    let runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            Box::pin(async move { Ok("done".to_string()) })
        };

    let deliver = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Ok("m1".to_string()) })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({"reactions": true}),
        path.clone(),
        store.clone(),
        deliver,
        runner,
    )
    .with_reaction_hook(hook.clone());

    // Test success sequence
    store
        .enqueue("msg_success", "42", "hello", None, 100)
        .unwrap();
    assert!(runtime.generate_one().await.unwrap());
    assert!(runtime.deliver_one().await.unwrap());

    let log = reactions_log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            (
                "add".to_string(),
                "msg_success".to_string(),
                "👀".to_string()
            ),
            (
                "remove".to_string(),
                "msg_success".to_string(),
                "👀".to_string()
            ),
            (
                "add".to_string(),
                "msg_success".to_string(),
                "✅".to_string()
            ),
        ]
    );

    // Test failure sequence
    reactions_log.lock().unwrap().clear();
    let failing_runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            Box::pin(async move { Err(gray_discord::runner::RunError::Timeout) })
        };
    let deliver2 = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Ok("m2".to_string()) })
    };
    let runtime_fail = gray_discord::gateway::Runtime::new(
        serde_json::json!({"reactions": true}),
        path,
        store.clone(),
        deliver2,
        failing_runner,
    )
    .with_reaction_hook(hook);

    store.enqueue("msg_fail", "42", "hello", None, 100).unwrap();
    assert!(runtime_fail.generate_one().await.unwrap());

    let log_fail = reactions_log.lock().unwrap().clone();
    assert_eq!(
        log_fail,
        vec![
            ("add".to_string(), "msg_fail".to_string(), "👀".to_string()),
            (
                "remove".to_string(),
                "msg_fail".to_string(),
                "👀".to_string()
            ),
            ("add".to_string(), "msg_fail".to_string(), "❌".to_string()),
        ]
    );
}

#[tokio::test]
async fn typing_cadence_throttles_within_8_seconds() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();

    let typing_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let typing_calls_clone = typing_calls.clone();
    let hook = std::sync::Arc::new(move |ch: u64| {
        typing_calls_clone.lock().unwrap().push(ch);
    });

    let fake_time = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let fake_time_clone = fake_time.clone();
    let clock = std::sync::Arc::new(move || {
        fake_time_clone.load(std::sync::atomic::Ordering::SeqCst) as f64
    });

    let dummy_runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            Box::pin(async move { Ok::<String, gray_discord::runner::RunError>("".to_string()) })
        };
    let dummy_deliver = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Ok::<String, String>("".to_string()) })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({}),
        path,
        store,
        dummy_deliver,
        dummy_runner,
    )
    .with_typing_hook(hook)
    .with_clock(clock);

    // t = 0: first tick triggers typing
    fake_time.store(0, std::sync::atomic::Ordering::SeqCst);
    runtime.report_progress("42").await;
    assert_eq!(typing_calls.lock().unwrap().len(), 1);

    // t = 3: within 8 seconds, throttled
    fake_time.store(3, std::sync::atomic::Ordering::SeqCst);
    runtime.report_progress("42").await;
    assert_eq!(typing_calls.lock().unwrap().len(), 1);

    // t = 9: elapsed 9 seconds >= 8s cadence, triggers second typing call
    fake_time.store(9, std::sync::atomic::Ordering::SeqCst);
    runtime.report_progress("42").await;
    assert_eq!(typing_calls.lock().unwrap().len(), 2);
    assert_eq!(*typing_calls.lock().unwrap(), vec![42, 42]);
}

#[tokio::test]
async fn reactions_disabled_when_config_flag_false() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();

    let reactions_log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let reactions_log_clone = reactions_log.clone();
    let hook = std::sync::Arc::new(move |op: &str, msg_id: &str, emoji: &str| {
        reactions_log_clone.lock().unwrap().push((
            op.to_string(),
            msg_id.to_string(),
            emoji.to_string(),
        ));
    });

    let runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            Box::pin(async move { Ok("done".to_string()) })
        };
    let deliver = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Ok("m1".to_string()) })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({"reactions": false}),
        path,
        store.clone(),
        deliver,
        runner,
    )
    .with_reaction_hook(hook);

    store
        .enqueue("msg_noreact", "42", "hello", None, 100)
        .unwrap();
    assert!(runtime.generate_one().await.unwrap());
    assert!(runtime.deliver_one().await.unwrap());

    assert!(reactions_log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn typing_indicator_disabled_when_config_flag_false() {
    // Port of Hermes' `discord.typing_indicator: false`. The gate sits in
    // the adapter before the typing RPC, so disabling it stops both the
    // REST poke and the host hook — the whole path, not one loop of it.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();

    let typing_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let typing_calls_clone = typing_calls.clone();
    let hook = std::sync::Arc::new(move |ch: u64| {
        typing_calls_clone.lock().unwrap().push(ch);
    });

    let fake_time = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let fake_time_clone = fake_time.clone();
    let clock = std::sync::Arc::new(move || {
        fake_time_clone.load(std::sync::atomic::Ordering::SeqCst) as f64
    });

    let dummy_runner =
        move |_config: &serde_json::Value, _path: &std::path::Path, _conv: &str, _prompt: &str| {
            Box::pin(async move { Ok::<String, gray_discord::runner::RunError>("".to_string()) })
        };
    let dummy_deliver = move |_part: gray_discord::durable::OutboxPart| {
        Box::pin(async move { Ok::<String, String>("".to_string()) })
    };

    let runtime = gray_discord::gateway::Runtime::new(
        serde_json::json!({"typing_indicator": false}),
        path,
        store,
        dummy_deliver,
        dummy_runner,
    )
    .with_typing_hook(hook)
    .with_clock(clock);

    assert!(!runtime.typing_enabled());
    // Two progress reports well past the 8s throttle window: still silent.
    fake_time.store(0, std::sync::atomic::Ordering::SeqCst);
    runtime.report_progress("42").await;
    fake_time.store(30, std::sync::atomic::Ordering::SeqCst);
    runtime.report_progress("42").await;
    assert!(
        typing_calls.lock().unwrap().is_empty(),
        "typing_indicator=false must silence the indicator entirely"
    );
}

#[tokio::test]
async fn typing_indicator_defaults_on_and_omitting_the_key_is_not_off() {
    // Default-on is the whole point of the port: a config written before
    // the flag existed keeps its indicator, and only an explicit false
    // silences it.
    let tmp = tempfile::tempdir().unwrap();
    for config in [serde_json::json!({}), serde_json::json!({"token": "x"})] {
        let store = Store::new(&tmp.path().join("queue.sqlite")).unwrap();
        let dummy_runner =
            move |_c: &serde_json::Value, _p: &std::path::Path, _v: &str, _q: &str| {
                Box::pin(async move { Ok::<String, gray_discord::runner::RunError>("".into()) })
            };
        let dummy_deliver = move |_p: gray_discord::durable::OutboxPart| {
            Box::pin(async move { Ok::<String, String>("".into()) })
        };
        let runtime = gray_discord::gateway::Runtime::new(
            config,
            tmp.path().join("config.json"),
            store,
            dummy_deliver,
            dummy_runner,
        );
        assert!(
            runtime.typing_enabled(),
            "typing is on unless explicitly disabled"
        );
    }
}
