use gray_discord::runner::{default_opts, run_gray, RunError, RunOpts};
use std::path::Path;

fn write_exe(path: &Path, script: &str) {
    std::fs::write(path, script).expect("fixture write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn base_config(exe: &Path, home: &Path) -> serde_json::Value {
    serde_json::json!({"gray_bin": exe.to_str().unwrap(), "gray_home": home.to_str().unwrap()})
}

fn gray_home(root: &Path) -> std::path::PathBuf {
    let home = root.join("gray-home");
    std::fs::create_dir(&home).unwrap();
    // Provider model "test" is a fixture; budget tests pin their own model.
    // These runner fixtures carry no `budget`, so no ledger is touched.
    gray_discord::config::atomic_json(
        &home.join("config.json"),
        &serde_json::json!({"model": "test"}),
    )
    .unwrap();
    home
}

#[tokio::test]
async fn nonzero_exit_is_failure_even_if_stdout_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let exe = root.join("fail");
    write_exe(&exe, "#!/bin/sh\necho partial-response\nexit 7\n");
    let config = base_config(&exe, &home);
    let err = run_gray(
        &config,
        &root.join("plugin.json"),
        "chat:one",
        "hello",
        default_opts(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("code 7"), "unexpected: {err}");
}

#[tokio::test]
async fn timeout_kills_child_and_releases_conversation_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let exe = root.join("sleep");
    write_exe(&exe, "#!/bin/sh\nsleep 60\n");
    let config = base_config(&exe, &home);
    for _ in 0..2 {
        let err = run_gray(
            &config,
            &root.join("plugin.json"),
            "chat:one",
            "hello",
            RunOpts {
                timeout_secs: Some(1),
                ..default_opts()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RunError::Timeout), "unexpected: {err}");
    }
}

#[tokio::test]
async fn session_switch_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let exe = root.join("switcher");
    write_exe(
        &exe,
        "#!/bin/sh\necho '{\"protocol\":1,\"turn_id\":\"t\",\"type\":\"progress\",\"session_id\":\"11111111-1111-1111-1111-111111111111\"}'\necho '{\"protocol\":1,\"turn_id\":\"t\",\"type\":\"progress\",\"session_id\":\"22222222-2222-2222-2222-222222222222\"}'\n",
    );
    let config = base_config(&exe, &home);
    let err = run_gray(
        &config,
        &root.join("plugin.json"),
        "chat:one",
        "hello",
        default_opts(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RunError::Protocol(_)), "unexpected: {err}");
}

#[tokio::test]
async fn multi_terminal_rows_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let exe = root.join("double");
    write_exe(
        &exe,
        "#!/bin/sh\necho '{\"protocol\":1,\"turn_id\":\"t\",\"type\":\"result\",\"session_id\":\"11111111-1111-1111-1111-111111111111\",\"text\":\"one\"}'\necho '{\"protocol\":1,\"turn_id\":\"t\",\"type\":\"result\",\"session_id\":\"11111111-1111-1111-1111-111111111111\",\"text\":\"two\"}'\n",
    );
    let config = base_config(&exe, &home);
    let err = run_gray(
        &config,
        &root.join("plugin.json"),
        "chat:one",
        "hello",
        default_opts(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RunError::Protocol(_)), "unexpected: {err}");
}

#[tokio::test]
async fn busy_conversation_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let exe = root.join("waiter");
    // Hold the child running while a second turn targets the same
    // conversation; the lock must reject it without touching the child.
    write_exe(&exe, "#!/bin/sh\nsleep 30\n");
    let config = base_config(&exe, &home);
    let path = root.join("plugin.json");
    let first = tokio::spawn({
        let config = config.clone();
        let path = path.clone();
        async move { run_gray(&config, &path, "chat:one", "hello", default_opts()).await }
    });
    // Give the first turn time to take run.lock.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let err = run_gray(&config, &path, "chat:one", "second", default_opts())
        .await
        .unwrap_err();
    assert!(matches!(err, RunError::Busy), "unexpected: {err}");
    first.abort();
}

// Real-gray integration (no transcript scan — terminal NDJSON only):
// resume replays history, conversations stay isolated. Requires gray on PATH.
#[tokio::test]
#[ignore]
async fn real_gray_two_turn_resume_and_conversation_isolation() {
    let bin = std::env::var("GRAY_TEST_BIN").expect("GRAY_TEST_BIN must point at a gray binary");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("gray-home");
    std::fs::create_dir(&home).unwrap();
    gray_discord::config::atomic_json(
        &home.join("config.json"),
        &serde_json::json!({"model": "test-model", "api_key": "fixture"}),
    )
    .unwrap();
    let config = serde_json::json!({"gray_bin": bin, "gray_home": home.to_str().unwrap()});
    let path = root.join("plugin.json");
    let first = run_gray(
        &config,
        &path,
        "chat:one",
        "Remember a violet bicycle",
        default_opts(),
    )
    .await
    .expect("first turn");
    assert!(!first.trim().is_empty());
    let second = run_gray(
        &config,
        &path,
        "chat:one",
        "What did I ask you to remember?",
        default_opts(),
    )
    .await
    .expect("second turn");
    assert!(!second.trim().is_empty());
    let other = run_gray(
        &config,
        &path,
        "chat:two",
        "A different conversation",
        default_opts(),
    )
    .await
    .expect("other conversation");
    assert!(!other.trim().is_empty());
    // Three isolated conversation stores, like the Python's glob assertion.
    let conv: Vec<_> = std::fs::read_dir(root.join("conversations"))
        .unwrap()
        .collect();
    assert_eq!(conv.len(), 3);
}

/// A fake `gray` that records the argv it was handed, then answers with one
/// terminal row so the turn completes.
fn argv_capture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let out = root.join("argv.txt");
    let exe = root.join("capture");
    write_exe(
        &exe,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n             echo '{{\"protocol\":1,\"turn_id\":\"t\",\"type\":\"result\",\"session_id\":\"11111111-1111-1111-1111-111111111111\",\"text\":\"ok\"}}'\n",
            out.display()
        ),
    );
    (exe, out)
}

/// `/model set` must reach the child as one `--model` flag; a channel that
/// never picked a model must not have the flag appended at all.
#[tokio::test]
async fn a_model_override_reaches_the_gray_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let (exe, out) = argv_capture(root);
    let config = base_config(&exe, &home);
    let opts = RunOpts {
        model: Some("openai/gpt-5".to_string()),
        ..Default::default()
    };
    run_gray(
        &config,
        &root.join("plugin.json"),
        "chat:one",
        "hello",
        opts,
    )
    .await
    .unwrap();
    let argv = std::fs::read_to_string(&out).unwrap();
    assert!(argv.lines().any(|l| l == "--model"), "argv was:\n{argv}");
    assert!(
        argv.lines().any(|l| l == "openai/gpt-5"),
        "argv was:\n{argv}"
    );
    assert_eq!(argv.lines().filter(|l| *l == "--model").count(), 1);
}

#[tokio::test]
async fn no_override_means_no_model_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = gray_home(root);
    let (exe, out) = argv_capture(root);
    let config = base_config(&exe, &home);
    run_gray(
        &config,
        &root.join("plugin.json"),
        "chat:one",
        "hello",
        default_opts(),
    )
    .await
    .unwrap();
    let argv = std::fs::read_to_string(&out).unwrap();
    assert!(!argv.lines().any(|l| l == "--model"), "argv was:\n{argv}");
}
