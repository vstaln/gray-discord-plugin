//! The token check's parsers (fixture JSON) and its one network call
//! (a loopback stand-in that records the request).
use gray_discord::discord_check::*;
use serde_json::json;

fn fixture() -> serde_json::Value {
    json!({
        "id": "1234567890123456789",
        "name": "gray-app",
        "bot": {"id": "1234567890123456789", "username": "graybot"},
        "owner": {"id": "111111111111111111", "username": "alice"},
        "flags": (1u64 << 19) | (1u64 << 15),
        "approximate_guild_count": 2
    })
}

#[test]
fn the_invite_integer_is_the_named_bits() {
    assert_eq!(INVITE_PERMISSIONS, 309_240_908_864);
    assert_eq!(INVITE_PERMISSION_BITS.len(), 10);
}

#[test]
fn an_owned_app_parses_owner_intents_and_servers() {
    let check = parse_application(&fixture()).unwrap();
    assert_eq!(check.app_id, "1234567890123456789");
    assert_eq!(check.bot_name, "graybot");
    assert_eq!(
        check.owners,
        [("111111111111111111".to_string(), "alice".to_string())]
    );
    assert!(check.message_content, "the limited flag counts");
    assert!(check.server_members);
    assert_eq!(check.server_count, Some(2));
    assert_eq!(
        check.invite_url(),
        "https://discord.com/oauth2/authorize?client_id=1234567890123456789&scope=bot+applications.commands&permissions=309240908864&integration_type=0"
    );
    assert_eq!(
        check.bot_settings_url(),
        "https://discord.com/developers/applications/1234567890123456789/bot"
    );
}

#[test]
fn a_team_app_lists_only_accepted_members() {
    let app = json!({
        "id": "42",
        "name": "team-app",
        "owner": {"id": "999", "username": "team-placeholder"},
        "team": {"members": [
            {"membership_state": 2, "user": {"id": "1", "username": "alice"}},
            {"membership_state": 1, "user": {"id": "2", "username": "invited"}},
            {"membership_state": 2, "user": {"id": "3"}}
        ]},
        "flags": 0
    });
    let check = parse_application(&app).unwrap();
    assert_eq!(
        check.owners,
        [
            ("1".to_string(), "alice".to_string()),
            ("3".to_string(), "3".to_string())
        ]
    );
    assert!(!check.message_content && !check.server_members);
    assert_eq!(check.server_count, None);
    assert_eq!(check.bot_name, "team-app");
    assert!(parse_application(&json!({"name": "x"})).is_err());
}

#[test]
fn the_invite_line_changes_when_the_bot_is_in_no_server() {
    let mut check = parse_application(&fixture()).unwrap();
    assert!(invite_lines(&check)[0].starts_with("Invite link"));
    check.server_count = Some(0);
    assert!(invite_lines(&check)[0].contains("isn't in any server yet"));
}

#[test]
fn pastes_are_cleaned_and_app_ids_caught() {
    assert_eq!(clean_token("\u{201c}abc.def.ghi\u{201d}"), "abc.def.ghi");
    assert_eq!(clean_token("  abc\u{200b}.def  "), "abc.def");
    assert!(has_inner_break("abc\ndef"));
    assert!(token_shape_error("1234567890123456789")
        .unwrap()
        .contains("application ID"));
    assert!(token_shape_error("abc.def.ghi").is_none());
    assert_eq!(clean_user_ids("<@!2>, 3 ,user:4,,"), ["2", "3", "4"]);
}

#[test]
fn merging_only_adds() {
    let merged = merge_allowed(&["999".into(), "1".into()], &["1".into(), "2".into()]);
    assert_eq!(merged, ["999", "1", "2"]);
}

/// One-shot HTTP stand-in on a real port; hands back the raw request.
fn stand_in(
    status: &'static str,
    body: &'static str,
) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(Ok(mut s)) = listener.incoming().next() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    (format!("http://127.0.0.1:{port}/api/v10"), rx)
}

#[tokio::test]
async fn the_check_sends_a_bot_header_to_applications_me() {
    let (base, request) = stand_in(
        "200 OK",
        r#"{"id":"42","bot":{"username":"graybot"},"owner":{"id":"7","username":"alice"},"flags":524288,"approximate_guild_count":0}"#,
    );
    let rest = gray_discord::transport::Rest::new(&base, "tok.en.value");
    let check = check_bot_token(&rest).await.unwrap();
    assert_eq!(check.app_id, "42");
    assert!(check.message_content);
    assert_eq!(check.owners, [("7".to_string(), "alice".to_string())]);
    let request = request.recv().unwrap().to_ascii_lowercase();
    assert!(
        request.starts_with("get /api/v10/applications/@me"),
        "{request}"
    );
    assert!(
        request.contains("authorization: bot tok.en.value"),
        "{request}"
    );
    assert!(!request.contains("bearer"), "{request}");
}

#[tokio::test]
async fn a_401_is_a_rejection_and_other_codes_are_unverified() {
    let (base, _rx) = stand_in("401 Unauthorized", r#"{"message":"401: Unauthorized"}"#);
    let rest = gray_discord::transport::Rest::new(&base, "sk-SECRET.part.two");
    let err = check_bot_token(&rest).await.unwrap_err();
    assert_eq!(err, CheckError::Rejected);
    assert!(!err.to_string().contains("sk-SECRET"));
    let (base, _rx) = stand_in("503 Service Unavailable", "{}");
    let rest = gray_discord::transport::Rest::new(&base, "x");
    assert_eq!(
        check_bot_token(&rest).await.unwrap_err(),
        CheckError::Status(503)
    );
}
