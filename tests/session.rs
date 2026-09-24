use gray_discord::session::{conversation_key, reset_home, ResetPolicy};
use serde_json::json;
use std::path::Path;

#[test]
fn dms_share_by_channel_but_groups_are_per_user() {
    assert_eq!(conversation_key("42", "7", true), "chat:42");
    assert_eq!(conversation_key("42", "7", false), "chat:42:user:7");
    assert_ne!(
        conversation_key("42", "7", false),
        conversation_key("42", "8", false)
    );
}

#[test]
fn idle_policy_resets_only_after_the_configured_window() {
    let policy = ResetPolicy {
        mode: "idle".into(),
        idle_minutes: 60,
        at_hour: 4,
    };
    let state = json!({"session_id": "old", "last_activity": 1_000.0});
    assert!(policy
        .reset_reason(&state, true, 1_000.0 + 59.0 * 60.0)
        .is_none());
    assert_eq!(
        policy.reset_reason(&state, true, 1_000.0 + 60.0 * 60.0),
        Some(gray_discord::session::ResetReason::Idle)
    );
    assert!(policy
        .reset_reason(&state, false, 1_000.0 + 60.0 * 60.0)
        .is_none());
}

#[test]
fn daily_policy_resets_after_the_local_boundary() {
    use chrono::{Local, TimeZone};
    let boundary = Local
        .from_local_datetime(
            &Local::now()
                .date_naive()
                .and_hms_opt(4, 0, 0)
                .expect("valid local boundary"),
        )
        .single()
        .expect("local boundary exists");
    let policy = ResetPolicy {
        mode: "daily".into(),
        idle_minutes: 1_440,
        at_hour: 4,
    };
    let state = json!({
        "session_id": "old",
        "last_activity": (boundary.timestamp() - 1) as f64
    });
    assert_eq!(
        policy.reset_reason(&state, true, (boundary.timestamp() + 3_600) as f64),
        Some(gray_discord::session::ResetReason::Daily)
    );
}

#[test]
fn missing_activity_is_a_one_time_legacy_reset() {
    let policy = ResetPolicy {
        mode: "both".into(),
        idle_minutes: 1_440,
        at_hour: 4,
    };
    assert_eq!(
        policy.reset_reason(&json!({"session_id": "old"}), true, 10_000.0),
        Some(gray_discord::session::ResetReason::Legacy)
    );
}

#[test]
fn reset_home_removes_the_old_transcript_and_advances_the_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("conversation");
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(sessions.join("archive")).unwrap();
    std::fs::write(sessions.join("old.jsonl"), "old transcript").unwrap();
    std::fs::write(sessions.join("archive").join("old-copy.jsonl"), "archive").unwrap();
    let state_path = home.join("session.json");
    std::fs::write(
        &state_path,
        serde_json::to_string(&json!({
            "generation": "old-generation",
            "session_id": "old",
            "last_activity": 1.0
        }))
        .unwrap(),
    )
    .unwrap();

    reset_home(Path::new(&home), 123.0).unwrap();

    assert!(!sessions.join("old.jsonl").exists());
    assert!(!sessions.join("archive").join("old-copy.jsonl").exists());
    assert!(!gray_discord::session::generation_is_current(
        &state_path,
        "old-generation"
    ));
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
    assert_ne!(state["generation"], "old-generation");
    assert!(state.get("session_id").is_none());
    assert_eq!(state["last_activity"], 123.0);
}
