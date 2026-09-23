//! Chat-bound cron: the binding the runner hands the child, and the
//! delivery the gateway posts back.

use gray_discord::cron;
use serde_json::json;

fn write_exe(path: &std::path::Path, script: &str) {
    std::fs::write(path, script).expect("fixture write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn the_route_becomes_the_origin_env() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    assert!(cron::origin_env(home).is_none(), "no route, no binding");
    cron::write_route(home, "1234567890", "4f3c2b1a");
    let raw = cron::origin_env(home).expect("bound");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["platform"], "discord");
    assert_eq!(v["chat"], "4f3c2b1a");
    assert_eq!(v["route"], "1234567890");
}

#[test]
fn an_incomplete_route_is_not_a_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    for partial in [
        json!({"platform": "discord", "chat": "a"}),
        json!({"platform": "discord", "route": "1"}),
        json!({"chat": "a", "route": "1"}),
        json!({"platform": "", "chat": "a", "route": "1"}),
        json!({"platform": "discord", "chat": " ", "route": "1"}),
    ] {
        std::fs::write(home.join("route.json"), partial.to_string()).unwrap();
        assert!(cron::origin_env(home).is_none(), "{partial}");
    }
}

#[test]
fn only_conversations_with_jobs_and_a_route_are_ticked() {
    let tmp = tempfile::tempdir().unwrap();
    let conversations = tmp.path();
    let mk = |name: &str, cron_dir: bool, route: bool| {
        let home = conversations.join(name);
        std::fs::create_dir_all(&home).unwrap();
        if cron_dir {
            std::fs::create_dir_all(home.join("cron")).unwrap();
        }
        if route {
            cron::write_route(&home, "1", name);
        }
        home
    };
    mk("both", true, true);
    mk("route-only", false, true);
    mk("cron-only", true, false);
    mk("neither", false, false);
    let homes = cron::routable_homes(conversations);
    assert_eq!(homes.len(), 1, "{homes:?}");
    assert!(homes[0].ends_with("both"));
}

#[test]
fn tick_output_routes_by_channel_and_ignores_everything_else() {
    let stdout = concat!(
        r#"{"type":"cron_delivery","job_id":"j1","name":"nightly","platform":"discord","chat":"4f3c","route":"123","text":"Cronjob Response: nightly\n(job_id: j1)"}"#,
        "\n",
        r#"{"type":"cron_tick","fired":1,"errors":0}"#,
        "\n",
        "not json at all\n",
        r#"{"type":"cron_delivery","job_id":"j2","platform":"telegram","chat":"5","route":"5","text":"nope"}"#,
        "\n",
        r#"{"type":"cron_delivery","job_id":"j3","platform":"discord","chat":"4f3c","route":"","text":"no route"}"#,
        "\n",
    );
    let got = cron::parse_tick(stdout);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].job_id, "j1");
    assert_eq!(got[0].channel, "123");
    assert!(got[0].text.starts_with("Cronjob Response:"));
}

#[tokio::test]
async fn a_tick_posts_back_through_gray_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("cron")).unwrap();
    cron::write_route(&home, "1234567890", "4f3c");
    let exe = tmp.path().join("gray");
    write_exe(
        &exe,
        r#"#!/bin/sh
[ "$1" = "cron" ] && [ "$2" = "tick" ] || exit 9
[ -f "$GRAY_HOME/route.json" ] || exit 8
cat <<'JSON'
{"type":"cron_delivery","job_id":"j1","name":"nightly","platform":"discord","chat":"4f3c","route":"1234567890","text":"Cronjob Response: nightly"}
{"type":"cron_tick","fired":1,"errors":0}
JSON
"#,
    );
    let got = cron::tick_home(&exe, &home).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].channel, "1234567890");
}

#[tokio::test]
async fn a_failing_tick_is_not_a_delivery() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("gray");
    write_exe(&exe, "#!/bin/sh\nexit 1\n");
    assert!(cron::tick_home(&exe, tmp.path()).await.is_empty());
    assert!(cron::tick_home(&tmp.path().join("missing"), tmp.path())
        .await
        .is_empty());
}

#[tokio::test]
async fn a_turn_hands_its_cron_binding_to_the_child() {
    // The whole point: the model runs a plain `gray cron add` and the job
    // still comes back to this channel.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("gray-home");
    std::fs::create_dir_all(&home).unwrap();
    gray_discord::config::atomic_json(&home.join("config.json"), &json!({"model": "test"}))
        .unwrap();
    let seen = root.join("seen-origin");
    let exe = root.join("gray");
    write_exe(
        &exe,
        &format!(
            r#"#!/bin/sh
printf '%s' "$GRAY_CRON_ORIGIN" > {}
cat <<'JSON'
{{"protocol":1,"turn_id":"t","session_id":"11111111-1111-1111-1111-111111111111","type":"result","text":"ok","usage":{{}}}}
JSON
"#,
            seen.display()
        ),
    );
    let config = json!({
        "gray_bin": exe.to_str().unwrap(),
        "gray_home": home.to_str().unwrap()
    });
    // The runner lays out the conversation home under the config's parent;
    // bind it the way the gateway does before the turn.
    let config_path = root.join("plugin.json");
    let conv_home = root
        .join("conversations")
        .join(gray_discord::runner::hex_sha256(b"chat:one"));
    std::fs::create_dir_all(&conv_home).unwrap();
    cron::write_route(&conv_home, "1234567890", "4f3c2b1a");

    let answer = gray_discord::runner::run_gray(
        &config,
        &config_path,
        "chat:one",
        "remind me hourly",
        gray_discord::runner::default_opts(),
    )
    .await
    .unwrap();
    assert_eq!(answer, "ok");
    let origin: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(origin["platform"], "discord");
    assert_eq!(origin["route"], "1234567890");
}
