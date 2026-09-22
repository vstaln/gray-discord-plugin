use gray_discord::setup::{
    invite, run_wired, DmStep, LoginStep, PairingFut, Prompter, VerifyStep, Wiring,
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

fn stub_login(_token: &str) -> gray_discord::setup::LoginFut {
    Box::pin(async { Ok("999".to_string()) })
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
        login: &stub_login as &LoginStep,
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
        "https://discord.com/oauth2/authorize?client_id=999&scope=bot&permissions=68608"
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
async fn hermes_path_is_token_plus_your_id() {
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
async fn doctor_disagreement_is_reported_but_the_config_stands() {
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(true, &["111"], &["FIXTURETOKEN"]);
    let path = tmp.path().join("config.json");
    assert!(run_offline(
        &path,
        &mut io,
        &wiring_bad_doctor(&pairing_ok, tmp.path()),
        false
    )
    .await
    .unwrap());
    // The save is never rolled back — the operator fixes the intent and
    // re-runs the doctor; but nothing claims success.
    assert!(path.exists());
    assert!(io
        .log
        .iter()
        .any(|l| l.contains("Doctor disagreed") && l.contains("Message Content Intent")));
    assert!(!io.log.iter().any(|l| l.contains("Doctor verified")));
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
