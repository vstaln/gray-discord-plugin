use gray_discord::discord_check::{BotCheck, CheckError};
use gray_discord::setup::{
    invite, run_wired, CheckFut, CheckStep, DmStep, PairingFut, Prompter, VerifyStep, Wiring,
};
use std::collections::VecDeque;
use std::path::Path;

/// Scripted prompter: pops canned answers, records prompts. `tty: false`
/// reproduces the non-terminal guard without a real terminal.
struct Fake {
    tty: bool,
    answers: VecDeque<String>,
    hidden: VecDeque<String>,
    log: Vec<String>,
}

impl Fake {
    fn new(tty: bool, answers: &[&str], hidden: &[&str]) -> Self {
        Self {
            tty,
            answers: answers.iter().map(|s| s.to_string()).collect(),
            hidden: hidden.iter().map(|s| s.to_string()).collect(),
            log: Vec::new(),
        }
    }
}

impl Prompter for Fake {
    fn is_terminal(&self) -> bool {
        self.tty
    }
    fn prompt(&mut self, text: &str) -> Result<String, String> {
        self.log.push(text.to_string());
        self.answers
            .pop_front()
            .ok_or_else(|| "no more answers".to_string())
    }
    fn prompt_hidden(&mut self, text: &str) -> Result<String, String> {
        self.log.push(text.to_string());
        self.hidden
            .pop_front()
            .ok_or_else(|| "no more hidden answers".to_string())
    }
    fn confirm(&mut self, text: &str) -> Result<bool, String> {
        self.log.push(text.to_string());
        let answer = self
            .answers
            .pop_front()
            .ok_or_else(|| "no more answers".to_string())?;
        Ok(answer.trim().eq_ignore_ascii_case("y"))
    }
    fn print_line(&mut self, text: &str) {
        self.log.push(text.to_string());
    }
}

/// A bot Discord accepts: intent on, no owner reported (so the wizard asks
/// for the ID, as before the owner lookup existed).
fn bot(owners: &[(&str, &str)], message_content: bool) -> BotCheck {
    BotCheck {
        app_id: "42".to_string(),
        bot_name: "graybot".to_string(),
        owners: owners
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect(),
        message_content,
        server_members: false,
        server_count: Some(0),
    }
}

fn stub_check(_token: &str) -> CheckFut {
    Box::pin(async { Ok(bot(&[], true)) })
}

fn stub_dm(_token: &str, _owner: u64) -> gray_discord::setup::DmFut {
    Box::pin(async { Ok(222) })
}

fn verify_ok(_config: &serde_json::Value) -> gray_discord::setup::VerifyFut {
    Box::pin(async { Ok(()) })
}

fn verify_bad(_config: &serde_json::Value) -> gray_discord::setup::VerifyFut {
    Box::pin(async { Err("Enable Message Content Intent".to_string()) })
}

fn pairing_ok(_token: &str, code: &str) -> PairingFut {
    let code = code.to_string();
    Box::pin(async move {
        assert!(!code.is_empty(), "pairing code must be shown");
        Ok(("111".to_string(), "222".to_string()))
    })
}

fn pairing_fail(_token: &str, _code: &str) -> PairingFut {
    Box::pin(async {
        Err("Pairing failed or expired; check intent and DM permissions".to_string())
    })
}

fn wiring(pairing: &'static gray_discord::setup::WaitForPairing, home: &Path) -> Wiring {
    Wiring {
        check: &stub_check as &CheckStep,
        dm: &stub_dm as &DmStep,
        verify: &verify_ok as &VerifyStep,
        pairing,
        gray_bin: Some("/bin/true".to_string()),
        gray_home: Some(home.to_string_lossy().into_owned()),
    }
}

fn wiring_bad_doctor(pairing: &'static gray_discord::setup::WaitForPairing, home: &Path) -> Wiring {
    Wiring {
        verify: &verify_bad as &VerifyStep,
        ..wiring(pairing, home)
    }
}

async fn run_offline(path: &Path, io: &mut Fake, w: &Wiring, pair: bool) -> Result<bool, String> {
    run_wired(path, io, w, pair).await
}

#[test]
fn invite_url_is_exact() {
    assert_eq!(
        invite("999"),
        "https://discord.com/oauth2/authorize?client_id=999&scope=bot+applications.commands&permissions=309240908864&integration_type=0"
    );
}

#[tokio::test]
async fn non_terminal_refuses_hidden_token_input() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(false, &[], &[]);
    let err = run_offline(
        &tmp.path().join("config.json"),
        &mut io,
        &wiring(&pairing_ok, tmp.path()),
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(err, "Setup needs a terminal for hidden token input");
}

#[tokio::test]
async fn existing_config_decline_keeps_old_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    std::fs::write(&path, "{\"kept\":true}").unwrap();
    // First answer is the replace confirm ("n" → decline).
    let mut io = Fake::new(true, &["n"], &[]);
    assert!(
        !run_offline(&path, &mut io, &wiring(&pairing_ok, tmp.path()), false)
            .await
            .unwrap()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"kept\":true}");
}

#[tokio::test]
async fn default_path_is_token_plus_your_id() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &["111,333,444"], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    assert!(
        run_offline(&path, &mut io, &wiring(&pairing_ok, tmp.path()), false)
            .await
            .unwrap()
    );
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // owner is the first ID; the rest are the allowlist; the home channel is
    // the DM the wizard created (stub 222) — never asked, never hunted.
    assert_eq!(saved["owner_id"], "111");
    assert_eq!(saved["channel_id"], "222");
    assert_eq!(saved["allowed_users"][0], "333");
    assert_eq!(saved["allowed_users"][1], "444");
    assert_eq!(saved["gray_bin"], "/bin/true");
    // Budget is not part of the wizard; validate_config would reject a null.
    assert!(saved.get("budget").is_none());
    // The doctor ran and agreed; the token never appeared anywhere.
    assert!(io.log.iter().any(|l| l.contains("Doctor verified")));
    assert!(io.log.iter().all(|l| !l.contains("FIXTURETOKEN")));
    gray_discord::config::load_config(&path).expect("saved config must validate");
    // Exactly two questions were asked: the token and your IDs.
    assert_eq!(
        io.log
            .iter()
            .filter(|l| l.contains("(hidden)") || l.contains("comma-separated"))
            .count(),
        2
    );
}

#[tokio::test]
async fn pair_path_discovers_the_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &[], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    assert!(
        run_offline(&path, &mut io, &wiring(&pairing_ok, tmp.path()), true)
            .await
            .unwrap()
    );
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["owner_id"], "111");
    assert_eq!(saved["channel_id"], "222");
    assert!(saved.get("allowed_users").is_none());
    assert!(io
        .log
        .iter()
        .any(|l| l.contains("one-time code to the bot")));
    assert!(io.log.iter().all(|l| !l.contains("FIXTURETOKEN")));
}

#[tokio::test]
async fn pair_failure_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &[], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    let err = run_offline(&path, &mut io, &wiring(&pairing_fail, tmp.path()), true)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "Pairing failed or expired; check intent and DM permissions"
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn empty_id_list_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &["   "], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    let err = run_offline(&path, &mut io, &wiring(&pairing_ok, tmp.path()), false)
        .await
        .unwrap_err();
    assert_eq!(err, "At least your own user ID is required");
    assert!(!path.exists());
}

#[tokio::test]
async fn non_snowflake_id_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &["not-an-id,222"], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    let err = run_offline(&path, &mut io, &wiring(&pairing_ok, tmp.path()), false)
        .await
        .unwrap_err();
    assert_eq!(err, "User IDs must be Discord snowflakes");
    assert!(!path.exists());
}

#[tokio::test]
async fn doctor_disagreement_stops_setup_but_the_config_stands() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &["111"], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    let err = run_offline(
        &path,
        &mut io,
        &wiring_bad_doctor(&pairing_ok, tmp.path()),
        false,
    )
    .await
    .unwrap_err();
    // The save is never rolled back — the operator fixes the intent and
    // re-runs the doctor; but setup fails, so no service starts on it.
    assert!(path.exists());
    assert!(err.contains("doctor disagreed") && err.contains("Message Content Intent"));
    assert!(!io.log.iter().any(|l| l.contains("Doctor verified")));
}

#[tokio::test]
async fn rerun_keeps_hand_tuned_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"token":"OLD","channel_id":"9","budget":{"daily_usd":3,"turn_usd":1,"input_per_million":1,"output_per_million":2,"model":"fixture"},"session_reset":{"mode":"idle","idle_minutes":5}}"#,
    )
    .unwrap();
    let mut io = Fake::new(true, &["y", "111"], &["FIXTURETOKEN"]);
    assert!(
        run_offline(&path, &mut io, &wiring(&pairing_ok, tmp.path()), false)
            .await
            .unwrap()
    );
    let s = saved(&path);
    assert_eq!(s["token"], "FIXTURETOKEN");
    assert_eq!(s["budget"]["daily_usd"], 3);
    assert_eq!(s["session_reset"]["mode"], "idle");
}

#[test]
fn cli_setup_registers_and_optionally_installs() {
    // Ports test_setup_cli.py: register on Ok(true) + install prompt default.
    // The CLI arm reads the answer via `prompt` and decides with
    // `cli::install_confirmed` (Python: `strip().lower() in ('', 'y', 'yes')`).
    for (answer, start) in [
        ("y", true),
        ("", true),
        ("yes", true),
        ("YES", true),
        ("n", false),
        ("no", false),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        let config = serde_json::json!({"gray_home": tmp.path().to_str().unwrap()});
        gray_discord::config::atomic_json(&path, &config).unwrap();
        let mut io = Fake::new(true, &[answer], &[]);
        // setup returned true -> register the outgoing tool.
        gray_discord::cli::register(&config, &path).expect("register");
        // CLI setup arm prompt -> install decision.
        let got = io
            .prompt("Enable and start the background service now? [Y/n] ")
            .unwrap();
        let installed = gray_discord::cli::install_confirmed(&got);
        assert_eq!(installed, start, "answer {answer:?}");
        let data: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("plugins/lock.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(data["plugins"]["discord"]["adapter_version"], "1.1");
    }
}

// --- The token is checked with Discord before anything is written. ---

fn with_check(check: &'static CheckStep, home: &Path) -> Wiring {
    Wiring {
        check,
        ..wiring(&pairing_ok, home)
    }
}

fn saved(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Only `good.token` is a real token; the owner is @alice (ID 1).
fn alice_check(token: &str) -> CheckFut {
    let ok = token == "good.token";
    Box::pin(async move {
        if ok {
            Ok(bot(&[("1", "alice")], true))
        } else {
            Err(CheckError::Rejected)
        }
    })
}

#[tokio::test]
async fn a_rejected_token_is_reasked_and_the_owner_is_merged_into_the_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    std::fs::write(&path, r#"{"allowed_users":["999"]}"#).unwrap();
    // Replace? y · allow yourself? Enter · others? Enter.
    let mut io = Fake::new(
        true,
        &["y", "", ""],
        // A wrong token, then the real one wrapped in rich-text curly quotes.
        &["not.the.token", "\u{201c}good.token\u{201d}"],
    );
    let w = with_check(&alice_check, tmp.path());
    assert!(run_offline(&path, &mut io, &w, false).await.unwrap());
    let data = saved(&path);
    assert_eq!(data["token"], "good.token", "cleaned before the save");
    assert_eq!(data["owner_id"], "1");
    assert_eq!(data["allowed_users"], serde_json::json!(["999", "1"]));
    let log = io.log.join("\n");
    assert!(log.contains("Discord rejected that token"), "{log}");
    assert!(log.contains("You are allowlisted (@alice)"), "{log}");
    assert!(log.contains("permissions=309240908864"), "{log}");
    assert!(log.contains("isn't in any server yet"), "{log}");
    assert!(!log.contains("not.the.token") && !log.contains("good.token"));
    // No Developer Mode hunt: the ID question never came.
    assert!(!log.contains("Your Discord user ID"), "{log}");
    gray_discord::config::load_config(&path).expect("saved config must validate");
}

fn always_reject(_token: &str) -> CheckFut {
    Box::pin(async { Err(CheckError::Rejected) })
}

#[tokio::test]
async fn three_rejected_tokens_save_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let mut io = Fake::new(true, &[], &["a.b.c", "d.e.f", "g.h.i"]);
    let err = run_offline(
        &path,
        &mut io,
        &with_check(&always_reject, tmp.path()),
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        "Discord rejected three tokens in a row; nothing was saved."
    );
    assert!(!path.exists());
}

fn never_numeric(token: &str) -> CheckFut {
    assert!(
        !token.bytes().all(|b| b.is_ascii_digit()),
        "an application ID must never be sent to Discord"
    );
    Box::pin(async { Ok(bot(&[], true)) })
}

#[tokio::test]
async fn a_numeric_app_id_paste_is_refused_once_with_guidance() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let mut io = Fake::new(true, &["111"], &["1234567890123456789", "real.bot.token"]);
    assert!(run_offline(
        &path,
        &mut io,
        &with_check(&never_numeric, tmp.path()),
        false
    )
    .await
    .unwrap());
    assert_eq!(saved(&path)["token"], "real.bot.token");
    assert!(io.log.iter().any(|l| l.contains("application ID")));
}

static INTENT_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Intent off on the first look, on after the operator saves the toggle.
fn intent_flips(_token: &str) -> CheckFut {
    let n = INTENT_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Box::pin(async move { Ok(bot(&[], n >= 1)) })
}

#[tokio::test]
async fn intent_off_links_the_toggle_and_enter_rechecks() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    // Enter re-checks, then your ID.
    let mut io = Fake::new(true, &["", "111"], &["good.token"]);
    assert!(run_offline(
        &path,
        &mut io,
        &with_check(&intent_flips, tmp.path()),
        false
    )
    .await
    .unwrap());
    let log = io.log.join("\n");
    assert!(
        log.contains("https://discord.com/developers/applications/42/bot"),
        "{log}"
    );
    assert!(log.contains("Privileged Gateway Intents"), "{log}");
    assert!(log.contains("Message Content Intent is on."), "{log}");
    assert_eq!(INTENT_CALLS.load(std::sync::atomic::Ordering::SeqCst), 2);
}

static SKIP_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn intent_stays_off(_token: &str) -> CheckFut {
    SKIP_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Box::pin(async { Ok(bot(&[], false)) })
}

#[tokio::test]
async fn skip_keeps_going_with_the_intent_still_off() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let mut io = Fake::new(true, &["skip", "111"], &["good.token"]);
    assert!(run_offline(
        &path,
        &mut io,
        &with_check(&intent_stays_off, tmp.path()),
        false
    )
    .await
    .unwrap());
    assert_eq!(SKIP_CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(path.exists());
}

fn offline(_token: &str) -> CheckFut {
    Box::pin(async {
        Err(CheckError::Unreachable(
            "Discord request failed; check connectivity".into(),
        ))
    })
}

#[tokio::test]
async fn offline_keeps_the_token_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let mut io = Fake::new(true, &["111"], &["good.token"]);
    assert!(
        run_offline(&path, &mut io, &with_check(&offline, tmp.path()), false)
            .await
            .unwrap()
    );
    assert_eq!(saved(&path)["token"], "good.token");
    let log = io.log.join("\n");
    assert!(log.contains("keeping it anyway"), "{log}");
    assert!(
        log.contains("Message Content Intent is on (Bot page"),
        "{log}"
    );
}

#[tokio::test]
async fn an_existing_owner_stays_and_the_detected_one_is_only_added() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    std::fs::write(&path, r#"{"owner_id":"555","allowed_users":["999"]}"#).unwrap();
    let mut io = Fake::new(true, &["y", "y", "<@!333>"], &["good.token"]);
    assert!(
        run_offline(&path, &mut io, &with_check(&alice_check, tmp.path()), false)
            .await
            .unwrap()
    );
    let data = saved(&path);
    assert_eq!(data["owner_id"], "555");
    assert_eq!(
        data["allowed_users"],
        serde_json::json!(["999", "1", "333"])
    );
}

#[tokio::test]
async fn declining_the_owner_offer_falls_back_to_typing_an_id() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    let mut io = Fake::new(true, &["n", "111"], &["good.token"]);
    assert!(
        run_offline(&path, &mut io, &with_check(&alice_check, tmp.path()), false)
            .await
            .unwrap()
    );
    let data = saved(&path);
    assert_eq!(data["owner_id"], "111");
    assert!(data.get("allowed_users").is_none());
}
