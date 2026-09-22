//! Port of gray_discord/setup.py: interactive owner pairing.
//! Credentials stay in the wizard; the model never receives the token.
use serde_json::{json, Value};
use std::path::Path;

/// Exact OAuth2 invite URL (bot scope, view/send/history permissions).
pub fn invite(app_id: &str) -> String {
    format!("https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot&permissions=68608")
}

/// Terminal IO. The real implementation talks to stdin/stderr; tests inject
/// a scripted fake. `print_line` goes to user-visible output (never logs).
pub trait Prompter {
    fn is_terminal(&self) -> bool;
    fn prompt(&mut self, text: &str) -> Result<String, String>;
    fn prompt_hidden(&mut self, text: &str) -> Result<String, String>;
    fn confirm(&mut self, text: &str) -> Result<bool, String>;
    fn print_line(&mut self, text: &str);
}

/// Owner pairing result: `(owner_id, dm_channel_id)` from the DM wait.
pub type PairingResult = Result<(String, String), String>;
/// The pairing wait as a future: it holds a gateway connection open, so it
/// cannot be plain sync work.
pub type PairingFut = std::pin::Pin<Box<dyn std::future::Future<Output = PairingResult> + Send>>;
/// Injected wait for the owner's DM: receives the bot token and the printed
/// code, returns `(owner_id, dm_channel_id)`. Tests inject a scripted fake
/// (`&|_, _| Box::pin(async { .. })`); the CLI passes [`default_pairing`].
pub type WaitForPairing = dyn Fn(&str, &str) -> PairingFut;
/// Login step injected for tests (`default_login` in production).
pub type LoginStep = dyn Fn(&str) -> LoginFut;

/// Real terminal prompter (hidden input via `stty -echo`, no new deps).
pub struct Tty;

impl Prompter for Tty {
    fn is_terminal(&self) -> bool {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }
    fn prompt(&mut self, text: &str) -> Result<String, String> {
        use std::io::Write;
        let _ = std::io::stderr().write_all(text.as_bytes());
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|_| "cannot read terminal input".to_string())?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    }
    fn prompt_hidden(&mut self, text: &str) -> Result<String, String> {
        use std::io::Write;
        let _ = std::io::stderr().write_all(text.as_bytes());
        let _ = std::io::stderr().flush();
        stty_echo(false);
        let mut line = String::new();
        let res = std::io::stdin().read_line(&mut line);
        stty_echo(true);
        let _ = std::io::stderr().write_all(b"\n");
        res.map_err(|_| "cannot read terminal input".to_string())?;
        Ok(line.trim().to_string())
    }
    fn confirm(&mut self, text: &str) -> Result<bool, String> {
        Ok(self.prompt(text)?.trim().eq_ignore_ascii_case("y"))
    }
    fn print_line(&mut self, text: &str) {
        eprintln!("{text}");
    }
}

fn stty_echo(on: bool) {
    let arg = if on { "echo" } else { "-echo" };
    let _ = std::process::Command::new("stty")
        .arg(arg)
        .stdin(std::process::Stdio::inherit())
        .status();
}

/// Resolve the gray binary (`GRAY_BIN` env or `PATH` lookup).
fn resolve_gray(explicit: &str) -> Option<String> {
    let candidate = explicit.trim();
    if candidate.is_empty() {
        return None;
    }
    if candidate.contains('/') {
        let path = Path::new(candidate);
        if path.is_file() && is_executable(path) {
            return Some(candidate.to_string());
        }
        return None;
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(candidate);
        if full.is_file() && is_executable(&full) {
            return Some(full.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Run the setup wizard. `wait_for_pairing` polls gateway events for the
/// owner's DM; it receives the bot token and the printed code and returns
/// `(owner_id, dm_channel_id)`.
pub async fn run(
    path: &Path,
    io: &mut dyn Prompter,
    wait_for_pairing: &WaitForPairing,
) -> Result<bool, String> {
    run_with_login(path, io, wait_for_pairing, &default_login).await
}

/// Test seam: production login (30 s bound) vs stub returning an app id.
/// Keeps network out of unit tests; the CLI always passes `default_login`.
pub async fn run_with_login(
    path: &Path,
    io: &mut dyn Prompter,
    wait_for_pairing: &WaitForPairing,
    login: &LoginStep,
) -> Result<bool, String> {
    if !io.is_terminal() {
        return Err("Setup needs a terminal for hidden token input".to_string());
    }
    if path.exists() && !io.confirm("Replace existing configuration? [y/N] ")? {
        return Ok(false);
    }
    let default_gray = std::env::var("GRAY_BIN").unwrap_or_else(|_| "gray".to_string());
    let gray_answer = io.prompt(&format!("gray binary [{default_gray}]: "))?;
    let gray_input = gray_answer.trim();
    let gray_name = if gray_input.is_empty() {
        &default_gray
    } else {
        gray_input
    };
    let Some(gray) = resolve_gray(gray_name) else {
        return Err("Install gray first; executable not found".to_string());
    };
    let default_home = std::env::var("GRAY_HOME").unwrap_or_else(|_| "~/.gray".to_string());
    let home_answer = io.prompt(&format!("Gray provider home [{default_home}]: "))?;
    let home_text = home_answer.trim();
    let home_raw = if home_text.is_empty() {
        &default_home
    } else {
        home_text
    };
    let expanded = shellexpand_home(home_raw);
    let gray_home = Path::new(&expanded);
    let provider: Value = std::fs::read(gray_home.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let model = provider.get("model").and_then(Value::as_str).unwrap_or("");
    if model.is_empty() {
        return Err("Configure a model in gray before setup".to_string());
    }
    // Budget is opt-in accounting (gateway-side parity with `budget set`):
    // declining writes no policy, and the daemon then starts with no ledger.
    let mut policy: Option<Value> = None;
    if io.confirm("Set a daily spend budget now? [y/N] ")? {
        io.print_line(
            "Set a daily allowance and conservative model prices. Unknown pricing is not free.",
        );
        io.print_line(
            "Client limits stop subsequent requests; configure provider-side caps for a hard invoice limit.",
        );
        let daily = io.prompt("Daily budget USD: ")?;
        let turn = io.prompt("Per-turn budget USD: ")?;
        let input_pm = io.prompt("Input USD per million tokens (0 only if genuinely free): ")?;
        let output_pm = io.prompt("Output USD per million tokens (include reasoning): ")?;
        let offered = json!({
            "model": model,
            "daily_usd": daily.trim(),
            "turn_usd": turn.trim(),
            "input_per_million": input_pm.trim(),
            "output_per_million": output_pm.trim()
        });
        crate::budget::validate(&offered, model).map_err(|e| e.to_string())?;
        policy = Some(offered);
    }

    let token = io.prompt_hidden("Discord BOT token (hidden): ")?;
    let token = token.trim().to_string();
    let app_id = login(&token).await?;
    io.print_line(&format!("Invite your bot: {}", invite(&app_id)));
    io.print_line("Enable Message Content Intent in the Discord developer portal.");
    let _ = io.prompt("Press Enter after inviting the bot and enabling the intent. ")?;

    let pairing = crate::policy::Pairing::new(now_secs());
    io.print_line(&format!(
        "DM this one-time code to the bot within five minutes: {}",
        pairing.code
    ));
    let (owner, dm) = wait_for_pairing(&token, &pairing.code).await?;
    io.print_line(&format!("Candidate owner ID: {owner}"));
    if !io.confirm("Confirm this is your Discord account? [y/N] ")? {
        return Err("Pairing not confirmed; nothing saved".to_string());
    }
    let channel_answer = io.prompt(&format!("Home channel ID [your DM: {dm}]: "))?;
    let channel = channel_answer.trim();
    let channel = if channel.is_empty() {
        dm
    } else {
        channel.to_string()
    };
    if !crate::config::snowflake(&Value::String(channel.clone())) {
        return Err("Invalid channel ID".to_string());
    }
    // Python parity (`setup.py`): `Path(gray).absolute()`,
    // `gray_home.expanduser().resolve()`, `path.parent.resolve()` — the saved
    // config must hold absolute paths or `save_config` validation rejects it.
    let gray = absolutize(Path::new(&gray)).to_string_lossy().into_owned();
    let gray_home = absolutize(Path::new(&expanded));
    let workdir = path
        .parent()
        .map(|p| absolutize(p).to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    // A declined budget writes no key at all: an absent policy means "no
    // ledger" (gateway parity), and `validate_config` rejects a null one.
    let mut saved = json!({
        "token": token,
        "owner_id": owner,
        "channel_id": channel,
        "gray_bin": gray,
        "gray_home": gray_home.to_string_lossy(),
        "workdir": workdir
    });
    if let Some(policy) = policy {
        saved["budget"] = policy;
    }
    crate::config::save_config(path, &saved)?;
    io.print_line("Configuration saved privately.");
    Ok(true)
}

/// The production pairing wait: one bare gateway connection that accepts
/// exactly one DM carrying the printed code, then disconnects. Mirrors
/// gray_discord/setup.py's wait, which polled the live gateway for the
/// owner's DM; the code expires after 300 s (`policy::Pairing`).
pub fn default_pairing(token: &str, code: &str) -> PairingFut {
    use twilight_gateway::{EventTypeFlags, Intents, Shard, ShardId, StreamExt};
    use twilight_model::gateway::event::Event;

    let token = token.to_string();
    let code = code.to_string();
    Box::pin(async move {
        let intents = Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES | Intents::MESSAGE_CONTENT;
        let mut shard = Shard::new(ShardId::ONE, token, intents);
        let deadline = std::time::Duration::from_secs(300);
        let started = std::time::Instant::now();
        loop {
            let left = deadline.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            // Timeout, a fatal shard error, or a dropped stream all end the
            // wait the same way: the operator retries setup.
            let event =
                match tokio::time::timeout(left, shard.next_event(EventTypeFlags::MESSAGE_CREATE))
                    .await
                {
                    Ok(Some(Ok(event))) => event,
                    _ => break,
                };
            if let Event::MessageCreate(msg) = event {
                let m = &msg.0;
                // DMs only, never another bot, exact code match.
                if m.guild_id.is_none()
                    && !m.author.bot
                    && crate::policy::code_matches(&m.content, &code)
                {
                    return Ok((m.author.id.to_string(), m.channel_id.to_string()));
                }
            }
        }
        Err("Pairing failed or expired; check intent and DM permissions".to_string())
    })
}

/// Boxed login future: `Send` so `run_with_login` stays `Send` for tokio.
pub type LoginFut =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;

/// Production login: REST `GET /users/@me` with a 30 s bound (Python parity:
/// `asyncio.wait_for(bot.login(token), 30)`).
pub fn default_login(token: &str) -> LoginFut {
    let token = token.to_string();
    Box::pin(async move {
        let rest = crate::transport::Rest::production(&token);
        tokio::time::timeout(std::time::Duration::from_secs(30), rest.login())
            .await
            .map_err(|_| "Discord login timed out; check the token and connectivity".to_string())?
            .map_err(|e| e.to_string())
    })
}

/// Join with cwd when relative (no `canonicalize`: the target may not
/// exist yet). Mirrors `Path.absolute()` / non-strict `resolve()`.
fn absolutize(path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn shellexpand_home(raw: &str) -> String {
    // Python parity (`Path.expanduser`): bare `~` and `~/...` both expand.
    if raw == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).to_string_lossy().into_owned();
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", Path::new(&home).to_string_lossy());
        }
    }
    raw.to_string()
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
