use gray_discord::{config, policy, text};
use serde_json::json;

fn base(tmp: &std::path::Path) -> serde_json::Value {
    json!({"token": "fixture", "owner_id": "123456789", "channel_id": "987654321",
           "gray_bin": "/bin/true", "gray_home": tmp.join("gray"), "workdir": tmp.join("work")})
}

#[test]
fn private_config_and_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let data = base(tmp.path());
    config::save_config(&path, &data).unwrap();
    assert_eq!(config::load_config(&path).unwrap(), data);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(config::validate_config(&json!({})).is_err());
    std::fs::write(&path, "{bad").unwrap();
    assert!(config::load_config(&path)
        .unwrap_err()
        .contains("run setup"));
}

#[test]
fn unicode_split_preserves_all_text() {
    let text: String = "😀\nhello ".repeat(1200);
    let chunks = text::split_message(&text, 2000).unwrap();
    assert_eq!(chunks.concat(), text);
    assert!(chunks.iter().all(|c| text::utf16_len(c) <= 2000));
    assert!(text::split_message("😀", 1).is_err());
}

#[test]
fn owner_or_allowlisted_dm_or_mention() {
    let allowed = vec!["777".to_string()];
    assert_eq!(
        policy::incoming("123", "123", false, true, "hello", "456", &[]),
        Some("hello".into())
    );
    assert_eq!(
        policy::incoming("123", "123", false, false, "<@!456> hello", "456", &[]),
        Some("hello".into())
    );
    assert_eq!(
        policy::incoming("777", "123", false, true, "hi", "456", &allowed),
        Some("hi".into())
    );
    for (a, b, dm, t) in [
        ("999", false, true, "x"),
        ("123", true, true, "x"),
        ("123", false, false, "hello"),
        ("777", false, false, "hello"),
    ] {
        assert_eq!(policy::incoming(a, "123", b, dm, t, "456", &allowed), None);
    }
}

#[test]
fn pairing_expires_and_consumes_once() {
    let mut p = policy::Pairing::new(0.0);
    assert!(!p.accept("wrong", 1.0));
    let code = p.code.clone();
    assert!(p.accept(&code, 2.0));
    assert!(!p.accept(&code, 3.0));
    let mut q = policy::Pairing::new(0.0);
    let qc = q.code.clone();
    assert!(!q.accept(&qc, 301.0));
}

#[test]
fn slash_admission_checks_owner_and_allowlist() {
    let allowed = vec!["777".to_string()];
    // owner can invoke /ask with prompt
    assert_eq!(
        policy::slash_admission("123", "123", &allowed, "ask", Some("hello")),
        Some("hello".into())
    );
    // allowed user can invoke /ask with trimmed prompt
    assert_eq!(
        policy::slash_admission("777", "123", &allowed, "ask", Some("  what is this  ")),
        Some("what is this".into())
    );
    // unauthorized user rejected
    assert_eq!(
        policy::slash_admission("999", "123", &allowed, "ask", Some("hello")),
        None
    );
    // /ask without prompt or empty prompt rejected
    assert_eq!(
        policy::slash_admission("123", "123", &allowed, "ask", None),
        None
    );
    assert_eq!(
        policy::slash_admission("123", "123", &allowed, "ask", Some("   ")),
        None
    );
    // /reset, /status, /stop admitted for allowed users
    for cmd in ["reset", "status", "stop"] {
        assert_eq!(
            policy::slash_admission("123", "123", &allowed, cmd, None),
            Some(cmd.into())
        );
        assert_eq!(
            policy::slash_admission("777", "123", &allowed, cmd, None),
            Some(cmd.into())
        );
        assert_eq!(
            policy::slash_admission("999", "123", &allowed, cmd, None),
            None
        );
    }
}
