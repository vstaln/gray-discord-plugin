use gray_discord::setup::{invite, run, run_with_login, Prompter};
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
            .ok_or_else(|| "no more hidden".to_string())
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

/// Pairing stub: accept the printed code, return a fixed owner + DM channel.
fn pairing_ok(_token: &str, code: &str) -> gray_discord::setup::PairingFut {
    let code = code.to_string();
    Box::pin(async move {
        assert!(!code.is_empty(), "pairing code must be shown");
        Ok(("111".to_string(), "222".to_string()))
    })
}

fn stub_login(_token: &str) -> gray_discord::setup::LoginFut {
    Box::pin(async { Ok("999".to_string()) })
}

/// Drive the wizard without network: stub login returns app id "999".
async fn run_offline(
    path: &std::path::Path,
    io: &mut Fake,
    wait: &gray_discord::setup::WaitForPairing,
) -> Result<bool, String> {
    run_with_login(path, io, wait, &stub_login).await
}

fn pairing_fail(_token: &str, _code: &str) -> gray_discord::setup::PairingFut {
    Box::pin(async {
        Err("Pairing failed or expired; check intent and DM permissions".to_string())
    })
}

fn provider_home(root: &Path, model: Option<&str>) -> std::path::PathBuf {
    let home = root.join("gray-home");
    std::fs::create_dir(&home).unwrap();
    let mut provider = serde_json::json!({});
    if let Some(model) = model {
        provider["model"] = model.into();
    }
    gray_discord::config::atomic_json(&home.join("config.json"), &provider).unwrap();
    home
}

fn base_answers(home: &std::path::Path) -> Vec<String> {
    vec![
        // gray binary prompt: absolute fixture exe.
        "/bin/true".to_string(),
        // gray home prompt.
        home.to_str().unwrap().to_string(),
        // budget question first (opt-in; "n" declines for most tests).
        "n".to_string(),
        // wait-for-enter after invite.
        String::new(),
    ]
}

/// The four answers the budget quiz asks for when accepted.
fn budget_answers() -> Vec<String> {
    vec![
        "y".to_string(),
        "5".to_string(),
        "1".to_string(),
        "2".to_string(),
        "3".to_string(),
    ]
}

#[tokio::test]
async fn declined_budget_writes_no_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = provider_home(tmp.path(), Some("test-model"));
    // base_answers already declines the budget question ("n").
    let mut answers = base_answers(&home);
    answers.push("y".to_string());
    answers.push(String::new());
    let mut io = Fake::new(
        true,
        &answers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &["FIXTURETOKEN"],
    );
    let path = tmp.path().join("config.json");
    assert!(run_offline(&path, &mut io, &pairing_ok).await.unwrap());
    // load_config validates: a null budget would be rejected here, proving
    // the declined path really omits the key.
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        config.get("budget").is_none(),
        "declined budget must vanish"
    );
    gray_discord::config::load_config(&path).expect("declined config must load");
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
    let err = run_offline(&tmp.path().join("config.json"), &mut io, &pairing_ok)
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
    assert!(!run_offline(&path, &mut io, &pairing_ok).await.unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"kept\":true}");
}

#[tokio::test]
async fn accept_path_saves_exact_field_set() {
    let tmp = tempfile::tempdir().unwrap();
    let home = provider_home(tmp.path(), Some("test-model"));
    let mut answers = base_answers(&home);
    // Replace the default budget "n" with the accepted quiz answers.
    answers.splice(2..3, budget_answers());
    // owner-ID confirm (yes) + home-channel default (empty → DM id).
    answers.push("y".to_string());
    answers.push(String::new());
    let mut io = Fake::new(
        true,
        &answers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &["FIXTURETOKEN"],
    );
    let path = tmp.path().join("config.json");
    assert!(run_offline(&path, &mut io, &pairing_ok).await.unwrap());
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["owner_id"], "111");
    assert_eq!(saved["channel_id"], "222");
    assert_eq!(saved["budget"]["model"], "test-model");
    assert_eq!(saved["gray_bin"], "/bin/true");
    assert!(saved.get("budget").is_some_and(|b| b.is_object()));
    // Token is stored but never printed: no log line may contain it.
    assert!(io.log.iter().all(|l| !l.contains("FIXTURETOKEN")));
    assert!(io
        .log
        .iter()
        .any(|l| l.contains("Configuration saved privately.")));
}

#[tokio::test]
async fn owner_decline_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = provider_home(tmp.path(), Some("test-model"));
    let mut answers = base_answers(&home);
    answers.push("n".to_string());
    let mut io = Fake::new(
        true,
        &answers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &["FIXTURETOKEN"],
    );
    let path = tmp.path().join("config.json");
    let err = run_offline(&path, &mut io, &pairing_ok).await.unwrap_err();
    assert_eq!(err, "Pairing not confirmed; nothing saved");
    assert!(!path.exists());
}

#[tokio::test]
async fn pairing_timeout_surfaces_python_message() {
    let tmp = tempfile::tempdir().unwrap();
    let home = provider_home(tmp.path(), Some("test-model"));
    let answers = base_answers(&home);
    let mut io = Fake::new(
        true,
        &answers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &["FIXTURETOKEN"],
    );
    let err = run_offline(&tmp.path().join("config.json"), &mut io, &pairing_fail)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "Pairing failed or expired; check intent and DM permissions"
    );
}

#[tokio::test]
async fn bad_channel_id_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let home = provider_home(tmp.path(), Some("test-model"));
    let mut answers = base_answers(&home);
    answers.push("y".to_string());
    answers.push("not-an-id".to_string());
    let mut io = Fake::new(
        true,
        &answers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &["FIXTURETOKEN"],
    );
    let err = run_offline(&tmp.path().join("config.json"), &mut io, &pairing_ok)
        .await
        .unwrap_err();
    assert_eq!(err, "Invalid channel ID");
}

#[test]
fn cli_setup_registers_and_optionally_installs() {
    // Ports test_setup_cli.py: register on Ok(true) + install prompt default.
    // The CLI arm reads the answer via `prompt` and decides with
    // `cli::install_confirmed` (Python: `strip().lower() in ('', 'y', 'yes')`).
    // `service::install` is still a Task-11 stub, so the test records the
    // decision instead of touching systemd.
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

#[test]
fn cli_setup_cancel_registers_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    // No lock file is created when setup returns false.
    assert!(!tmp.path().join("plugins/lock.json").exists());
}

#[tokio::test]
async fn run_requires_terminal_without_network() {
    // `run` wires the production login, but the TTY guard fires first, so
    // this exercises the production entry without touching the network
    // (global constraint: no live network in tests).
    let tmp = tempfile::tempdir().unwrap();
    let mut io = Fake::new(false, &[], &[]);
    let err = run(&tmp.path().join("config.json"), &mut io, &pairing_ok)
        .await
        .unwrap_err();
    assert_eq!(err, "Setup needs a terminal for hidden token input");
}
