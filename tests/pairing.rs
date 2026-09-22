//! Discord-side pairing (the OpenClaw pattern): an unconfigured DMer is told
//! their own ID and a code; `gray discord pairing approve discord <code>`
//! admits them once.

use gray_discord::gateway::open_store;
use gray_discord::pairing::{approve, gen_code, reply_for, unconfigured_reply};
use gray_discord::setup::Prompter;
use serde_json::json;

fn config_with_home(tmp: &tempfile::TempDir, body: serde_json::Value) -> std::path::PathBuf {
    let path = tmp.path().join("config.json");
    gray_discord::config::atomic_json(&path, &body).unwrap();
    path
}

fn valid_config(tmp: &tempfile::TempDir, owner: Option<&str>) -> serde_json::Value {
    let mut c = json!({
        "token": "FAKE.TOKEN.NOTREAL",
        "owner_id": owner.unwrap_or("111"),
        "channel_id": "222",
        "gray_bin": "/bin/true",
        "gray_home": tmp.path().to_str().unwrap(),
        "workdir": tmp.path().to_str().unwrap(),
    });
    if owner.is_none() {
        c.as_object_mut().unwrap().remove("owner_id");
    }
    c
}

#[test]
fn codes_are_short_uppercase_and_unique() {
    for _ in 0..50 {
        let c = gen_code();
        assert_eq!(c.len(), 8);
        assert!(c
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()));
    }
    assert_ne!(gen_code(), gen_code());
}

#[test]
fn reply_names_the_command_with_the_code() {
    let text = unconfigured_reply("1493623750858375228", "SNU3ZQ37");
    assert!(text.contains("Your Discord user id: 1493623750858375228"));
    assert!(text.contains("Pairing code: SNU3ZQ37"));
    assert!(text.contains("gray discord pairing approve discord SNU3ZQ37"));
}

#[test]
fn only_human_dms_get_a_pairing_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open_store(&tmp.path().join("config.json")).unwrap();
    // Bots and guild traffic never see it.
    assert!(reply_for(&store, "111", true, true).is_none());
    assert!(reply_for(&store, "111", false, false).is_none());
    assert!(reply_for(&store, "", true, false).is_none());
    // A human DM gets a code.
    let reply = reply_for(&store, "333", true, false).expect("human DM pairs");
    assert!(reply.contains("Your Discord user id: 333"));
    assert!(reply.contains("pairing approve discord"));
}

#[test]
fn a_repeat_dm_reuses_the_pending_code() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open_store(&tmp.path().join("config.json")).unwrap();
    let first = reply_for(&store, "333", true, false).unwrap();
    let second = reply_for(&store, "333", true, false).unwrap();
    assert_eq!(first, second, "no code pile-up for a persistent asker");
}

#[test]
fn approve_adds_to_the_allowlist_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = config_with_home(&tmp, valid_config(&tmp, Some("111")));
    let store = open_store(&path).unwrap();
    store.pairing_insert("SNU3ZQ37", "333").unwrap();
    let note = approve(&path, "discord", "SNU3ZQ37").unwrap();
    assert!(note.contains("333"), "{note}");
    let saved: serde_json::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(saved["allowed_users"][0], "333");
    assert_eq!(saved["owner_id"], "111");
    // Single use: a replay finds nothing.
    let err = approve(&path, "discord", "SNU3ZQ37").unwrap_err();
    assert_eq!(err, "unknown or already-used pairing code");
}

#[test]
fn approve_on_an_ownerless_bot_makes_them_owner() {
    let tmp = tempfile::tempdir().unwrap();
    // An ownerless config: channel present, owner absent.
    let cfg = valid_config(&tmp, None);
    let path = tmp.path().join("config.json");
    gray_discord::config::atomic_json(&path, &cfg).unwrap();
    let store = open_store(&path).unwrap();
    store.pairing_insert("SNU3ZQ37", "444").unwrap();
    let note = approve(&path, "discord", "SNU3ZQ37").unwrap();
    assert!(note.contains("444") && note.contains("owner"), "{note}");
    let saved: serde_json::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(saved["owner_id"], "444");
}

#[test]
fn approve_refuses_other_platforms() {
    let tmp = tempfile::tempdir().unwrap();
    let path = config_with_home(&tmp, valid_config(&tmp, Some("111")));
    assert!(approve(&path, "slack", "SNU3ZQ37")
        .unwrap_err()
        .contains("unknown platform"));
}

#[test]
fn the_config_written_by_the_wizard_survives_pairing_approval() {
    // The whole loop the operator runs: wizard writes the file, a stranger
    // DMs the bot, the operator approves. Nothing here touches the network.
    let tmp = tempfile::tempdir().unwrap();
    let path = config_with_home(&tmp, valid_config(&tmp, Some("111")));
    let store = open_store(&path).unwrap();
    let reply = reply_for(&store, "555", true, false).unwrap();
    let code = reply
        .lines()
        .find(|l| l.starts_with("Pairing code: "))
        .and_then(|l| l.rsplit(' ').next())
        .unwrap()
        .to_string();
    approve(&path, "discord", &code).unwrap();
    let saved: serde_json::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(saved["allowed_users"][0], "555");
    gray_discord::config::load_config(&path).expect("config still validates");
}

/// The Fake prompter is in the setup tests; silence the unused import here.
#[allow(dead_code)]
fn _assert_prompter_in_scope(_p: &dyn Prompter) {}
