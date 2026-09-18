use std::process::Command;

#[test]
fn help_lists_subcommands() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let out = Command::new(exe).arg("--help").output().expect("spawn");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    for cmd in [
        "setup",
        "run",
        "sidecar",
        "register",
        "doctor",
        "schedule",
        "allowlist",
    ] {
        assert!(text.contains(cmd), "help missing {cmd}");
    }
}

#[test]
fn missing_config_fails_without_traceback() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    let out = Command::new(exe)
        .arg("--config")
        .arg(&missing)
        .arg("doctor")
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).expect("utf8");
    assert!(!err.contains("Traceback"), "stderr leaked: {err}");
}

#[tokio::test]
async fn sidecar_real_process_without_token() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    let mut child = tokio::process::Command::new(exe)
        .arg("--config")
        .arg(&missing)
        .arg("sidecar")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sidecar");
    let stdin = child.stdin.take().expect("stdin");
    let requests = serde_json::json!([
        {"id": 1, "method": "plugin/manifest"},
        {"id": 2, "method": "tool/call", "params": {"name": "unknown"}}
    ]);
    let mut input = String::new();
    for row in requests.as_array().unwrap() {
        input.push_str(&row.to_string());
        input.push('\n');
    }
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = stdin;
        stdin
            .write_all(input.as_bytes())
            .await
            .expect("write requests");
    }
    let out = child.wait_with_output().await.expect("wait");
    assert!(out.status.success(), "stderr: {:?}", out.stderr);
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .expect("utf8")
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .expect("response rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[0]["result"]["name"], "discord");
    assert_eq!(rows[1]["id"], 2);
    assert_eq!(rows[1]["result"]["is_error"], true);
}

#[tokio::test]
async fn delivery_failure_is_not_success_or_secret_disclosure() {
    // Config token is random per run; dispatch must never echo it back.
    let token = format!("PRIVATE-TOKEN-{}", uuid_like());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let config = serde_json::json!({
        "token": token,
        "owner_id": "123",
        "channel_id": "42",
        "gray_bin": "/bin/false",
        "gray_home": tmp.path().join("gray"),
        "workdir": tmp.path().join("work")
    });
    gray_discord::config::save_config(&path, &config).expect("save");
    let reply = gray_discord::sidecar::dispatch(
        "tool/call",
        &serde_json::json!({"name": "discord_send", "args": {"content": "hello"}}),
        &path,
    )
    .await;
    assert_eq!(reply["is_error"], true);
    assert!(
        !reply.to_string().contains(&token),
        "secret leaked: {reply}"
    );
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

#[test]
fn service_escapes_paths_and_does_not_embed_token() {
    let text =
        gray_discord::service::unit(std::path::Path::new("/tmp/percent% and space/config.json"))
            .expect("unit");
    assert!(text.contains("percent%% and space"), "unescaped: {text}");
    assert!(text.contains("KillMode=control-group"));
    assert!(!text.to_lowercase().contains("token"));
    assert!(gray_discord::service::quote("/tmp/new\nline").is_err());
}

#[test]
fn registration_preserves_other_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let lock = home.join("plugins/lock.json");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(
        &lock,
        serde_json::json!({"schema": 1, "plugins": {"other": {"enabled": false}}}).to_string(),
    )
    .unwrap();
    let config = serde_json::json!({"gray_home": home.to_str().unwrap()});
    gray_discord::cli::register(&config, &home.join("config.json")).expect("register");
    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
    assert_eq!(
        data["plugins"]["other"],
        serde_json::json!({"enabled": false})
    );
    // argv: [<current-exe>, sidecar, --config, <abs path>].
    assert_eq!(data["plugins"]["discord"]["argv"][1], "sidecar");
    // Double-register is idempotent (same argv, same path).
    gray_discord::cli::register(&config, &home.join("config.json")).expect("re-register");
    // A different config path targeting the same lock is refused.
    assert!(
        gray_discord::cli::register(&config, &home.join("different.json")).is_err(),
        "conflicting path must be refused"
    );
}
