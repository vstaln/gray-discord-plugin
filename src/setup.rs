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
/// `(owner_id, dm_channel_id)`. Only used when the caller asks for pairing.
pub struct Wiring {
    pub login: &'static LoginStep,
    /// Open the DM channel with a user (setup's home channel).
    pub dm: &'static DmStep,
    /// The app's own doctor, run after the config is written.
    pub verify: &'static VerifyStep,
    /// The gateway-code wait, for `--pair`.
    pub pairing: &'static WaitForPairing,
    /// Overrides for the silent gray-path resolution (`GRAY_BIN` / `GRAY_HOME`
    /// in production); tests pin them instead of mutating the environment.
    pub gray_bin: Option<String>,
    pub gray_home: Option<String>,
}

/// Everything setup needs from the outside world. Production defaults in
/// [`Wiring::production`]; tests inject stubs and keep the network out.
pub type DmStep = dyn Fn(&str, u64) -> DmFut;
pub type VerifyStep = dyn Fn(&Value) -> VerifyFut;

impl Wiring {
    pub fn production() -> Self {
        Self {
            login: &default_login,
            dm: &default_dm,
            verify: &default_verify,
            pairing: &default_pairing,
            gray_bin: None,
            gray_home: None,
        }
    }
}

/// The wizard. `pair` selects the DM-code dance for discovering the owner's
/// ID; the default path just asks for the ID (Hermes parity: token + your
/// user ID, done).
pub async fn run(path: &Path, io: &mut dyn Prompter, pair: bool) -> Result<bool, String> {
    let wiring = Wiring::production();
    run_wired(path, io, &wiring, pair).await
}

pub async fn run_wired(
    path: &Path,
    io: &mut dyn Prompter,
    wiring: &Wiring,
    pair: bool,
) -> Result<bool, String> {
    if !io.is_terminal() {
        return Err("Setup needs a terminal for hidden token input".to_string());
    }
    if path.exists() && !io.confirm("Replace existing configuration? [y/N] ")? {
        return Ok(false);
    }
    // Hermes parity: one secret, one identity. Gray's binary and home are
    // resolved silently (env, then the conventional defaults) — they are
    // derived facts, not questions.
    let gray = match &wiring.gray_bin {
        Some(g) => g.clone(),
        None => resolve_gray(&std::env::var("GRAY_BIN").unwrap_or_else(|_| "gray".to_string()))
            .ok_or_else(|| "Install gray first; executable not found".to_string())?,
    };
    let gray_home = match &wiring.gray_home {
        Some(h) => absolutize(Path::new(h)),
        None => absolutize(Path::new(&shellexpand_home(
            &std::env::var("GRAY_HOME").unwrap_or_else(|_| "~/.gray".to_string()),
        ))),
    };

    let token = io.prompt_hidden("Discord bot token (hidden): ")?;
    let token = token.trim().to_string();
    let app_id = (wiring.login)(&token).await?;
    io.print_line(&format!("Invite your bot: {}", invite(&app_id)));
    io.print_line("Enable Message Content Intent in the Discord developer portal.");

    // Who may talk to the bot: the owner (first ID) plus an optional
    // allowlist. The home channel is then the DM with the owner — created,
    // never asked for.
    let (owner, mut allowed) = if pair {
        let pairing = crate::policy::Pairing::new(now_secs());
        io.print_line(&format!(
            "DM this one-time code to the bot within five minutes: {}",
            pairing.code
        ));
        let (owner, dm) = (wiring.pairing)(&token, &pairing.code).await?;
        io.print_line(&format!("Owner ID: {owner} (home channel: your DM {dm})"));
        (owner, Vec::new())
    } else {
        let answer = io.prompt("Your Discord user ID (comma-separated to also allow others): ")?;
        let mut ids: Vec<String> = answer
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        anyhow_ids(&ids)?;
        let owner = ids.remove(0);
        (owner, ids)
    };
    if !crate::config::snowflake(&Value::String(owner.clone())) {
        return Err("User IDs must be Discord snowflakes".to_string());
    }
    allowed.retain(|id| crate::config::snowflake(&Value::String(id.clone())));

    let owner_num: u64 = owner
        .parse()
        .map_err(|_| "User IDs must be Discord snowflakes".to_string())?;
    let dm_channel = (wiring.dm)(&token, owner_num).await?;

    // Python parity (`setup.py`): `Path(gray).absolute()`,
    // `gray_home.expanduser().resolve()`, `path.parent.resolve()` — the saved
    // config must hold absolute paths or `save_config` validation rejects it.
    let workdir = path
        .parent()
        .map(|p| absolutize(p).to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let mut saved = json!({
        "token": token,
        "owner_id": owner,
        "channel_id": dm_channel.to_string(),
        "gray_bin": absolutize(Path::new(&gray)).to_string_lossy(),
        "gray_home": gray_home.to_string_lossy(),
        "workdir": workdir,
        "session_reset": {
            "mode": "both",
            "idle_minutes": 1_440,
            "at_hour": 4
        }
    });
    if !allowed.is_empty() {
        saved["allowed_users"] = Value::Array(allowed.into_iter().map(Value::String).collect());
    }
    crate::config::save_config(path, &saved)?;
    io.print_line("Configuration saved privately.");

    // Prove it with the app's own doctor before anyone is told it worked.
    let report = (wiring.verify)(&saved).await;
    match report {
        Ok(()) => io.print_line("Doctor verified the configuration."),
        Err(e) => io.print_line(&format!("Doctor disagreed (fix and re-run): {e}")),
    }
    Ok(true)
}

/// Snowflake check for every comma-separated ID, with the same error text.
fn anyhow_ids(ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Err("At least your own user ID is required".to_string());
    }
    if ids
        .iter()
        .any(|id| !crate::config::snowflake(&Value::String(id.clone())))
    {
        return Err("User IDs must be Discord snowflakes".to_string());
    }
    Ok(())
}

/// The production DM open: POST /users/@me/channels with a 30 s bound.
pub type DmFut = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, String>> + Send>>;

pub fn default_dm(token: &str, owner: u64) -> DmFut {
    let token = token.to_string();
    Box::pin(async move {
        let rest = crate::transport::Rest::production(&token);
        tokio::time::timeout(std::time::Duration::from_secs(30), rest.create_dm(owner))
            .await
            .map_err(|_| "Discord did not answer opening your DM".to_string())?
            .map_err(|e| e.to_string())
    })
}

/// The production doctor, bounded like the CLI's own (30 s).
pub type VerifyFut =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

pub fn default_verify(config: &Value) -> VerifyFut {
    let config = config.clone();
    Box::pin(async move {
        let token = config
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let rest = crate::transport::Rest::production(&token);
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::doctor::check(&config, &rest),
        )
        .await
        .map_err(|_| "doctor timed out after 30s".to_string())?
    })
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
