//! Doctor's guild-permission path, against the loopback stub. The member
//! object carries no `permissions` field, so the check must compute them
//! from roles — the old direct read failed every guild channel with a
//! bare HTTP 200.
mod common;

use gray_discord::transport::Rest;

fn config() -> serde_json::Value {
    // The doctor requires a gray home with a configured model; a tmpdir
    // satisfies it without touching the operator's real ~/. gray.
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":"fixture-model"}"#,
    )
    .unwrap();
    let home_path = home.path().to_string_lossy().into_owned();
    std::mem::forget(home); // the test process outlives the check
    serde_json::json!({
        "token": "TESTTOKEN",
        "channel_id": "42",
        "gray_bin": "/bin/true",
        "gray_home": home_path,
        "workdir": "/tmp",
    })
}

#[tokio::test]
async fn a_guild_channel_is_allowed_when_a_role_carries_the_perms() {
    let stub = common::Stub::start().await;
    *stub.channel_guild.lock().unwrap() = true;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    gray_discord::doctor::check(&config(), &rest)
        .await
        .expect("role union (68608) must satisfy doctor");
}

#[tokio::test]
async fn a_dm_home_channel_skips_the_guild_check() {
    let stub = common::Stub::start().await;
    let rest = Rest::new(&stub.base, "TESTTOKEN");
    gray_discord::doctor::check(&config(), &rest)
        .await
        .expect("DM channels carry no guild permissions to check");
}
