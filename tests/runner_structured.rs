use gray_discord::runner::{default_opts, run_gray_input, RunInput};
use serde_json::json;

#[tokio::test]
async fn structured_runner_uses_private_file_and_continues_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("gray-home");
    std::fs::create_dir(&home).unwrap();
    gray_discord::config::atomic_json(&home.join("config.json"), &json!({"model": "fixture"}))
        .unwrap();
    let capture = root.join("argv.log");
    let executable = root.join("gray-fixture");
    let capture_literal = capture.to_string_lossy().replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\ncapture='{capture_literal}'\nprintf '%s\\n' \"$@\" >> \"$capture\"\ninput=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = '--input-json' ]; then input=\"$arg\"; fi\n  prev=\"$arg\"\ndone\nif [ -n \"$input\" ]; then stat -c '%a' \"$input\" >> \"$capture\"; cat \"$input\" >> \"$capture\"; fi\nprintf '%s\\n' '{{\"protocol\":1,\"turn_id\":\"turn-1\",\"session_id\":\"11111111-1111-4111-8111-111111111111\",\"type\":\"result\",\"text\":\"ok\"}}'\n",
    );
    std::fs::write(&executable, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config = json!({
        "gray_bin": executable,
        "gray_home": home,
        "timeout_seconds": 30
    });
    let input = RunInput::Structured(json!({
        "protocol": "gray.discord.input",
        "version": 1,
        "kind": "component_event",
        "payload": {"action": "refresh", "values": {"id": "7"}}
    }));
    assert_eq!(
        run_gray_input(
            &config,
            &root.join("plugin.json"),
            "chat:one",
            input.clone(),
            default_opts(),
        )
        .await
        .unwrap(),
        "ok"
    );
    let first = std::fs::read_to_string(&capture).unwrap();
    assert!(first.contains("--input-json"));
    assert!(first.contains("600"));
    assert!(first.contains("gray.discord.input"));
    assert!(!first.contains("--print"));
    let path = first
        .lines()
        .skip_while(|line| *line != "--input-json")
        .nth(1)
        .expect("input path recorded");
    assert!(
        !std::path::Path::new(&path).exists(),
        "input file was not cleaned up"
    );

    assert_eq!(
        run_gray_input(
            &config,
            &root.join("plugin.json"),
            "chat:one",
            input,
            default_opts(),
        )
        .await
        .unwrap(),
        "ok"
    );
    let second = std::fs::read_to_string(&capture).unwrap();
    assert!(second.matches("--session").count() >= 1);
    assert!(
        second
            .matches("11111111-1111-4111-8111-111111111111")
            .count()
            >= 1
    );
}
