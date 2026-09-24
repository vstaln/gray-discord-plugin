//! Port of gray_discord/runner.py: isolated `gray -p --json` child per turn.
//! One conversation home per sha256(conversation); explicit session pointers
//! only — never guess a shared session id.
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::budget::{Budget, BudgetBlocked};

/// Cap on one NDJSON stdout line (matches the Python `limit=1024*1024`).
const LINE_CAP: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub enum RunError {
    Busy,
    Budget(BudgetBlocked),
    Timeout,
    Spawn(String),
    Protocol(String),
    Exit(Option<i32>),
    Incomplete,
    Empty,
    Io(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("This conversation is busy"),
            Self::Budget(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("gray turn timed out"),
            Self::Spawn(_) => f.write_str("cannot start gray agent"),
            Self::Protocol(_) => {
                f.write_str("Invalid agent JSON output; upgrade gray to a compatible version")
            }
            Self::Exit(Some(n)) => write!(
                f,
                "gray exited with code {n}; not retrying possible side effects"
            ),
            Self::Exit(None) => {
                f.write_str("gray exited without a status; not retrying possible side effects")
            }
            Self::Incomplete => f.write_str(
                "Agent did not return a completed result; actions may already have occurred",
            ),
            Self::Empty => f.write_str("Agent returned no final answer"),
            Self::Io(_) => f.write_str("gray turn failed; check connectivity"),
        }
    }
}
impl std::error::Error for RunError {}

/// Boxed progress callback: one `--json` progress row in, nothing out,
/// never fails. The whole row (phase + tool + redacted detail) so a
/// renderer can use everything gray emits without a second protocol.
pub type ProgressFn<'a> = Box<dyn FnMut(&Value) + Send + 'a>;

/// Per-turn options. `timeout_secs` defaults to `timeout_seconds` from config
/// (600 when absent); `progress` phases never fail the turn; `receipt` gets
/// the terminal row merged in, like the Python's `receipt.update(final)`.

#[derive(Default)]
pub struct RunOpts<'a> {
    pub timeout_secs: Option<u64>,
    pub progress: Option<ProgressFn<'a>>,
    pub receipt: Option<&'a mut Value>,
    /// Per-turn model override (`/model set`). Appended as `--model` only
    /// when present, so a channel that never picks one is unaffected.
    pub model: Option<String>,
}

/// Progress callback without a captured borrow: plain function pointer plus
/// an opaque `Send` payload. Lets the consume future own everything it
/// touches (the Python just awaits a closure; Rust needs the split).
pub struct ProgressCb<'a> {
    pub call: fn(&mut ProgressCtx<'a>, &Value),
    pub ctx: ProgressCtx<'a>,
}

pub struct ProgressCtx<'a> {
    pub inner: Option<ProgressFn<'a>>,
}

pub fn default_opts() -> RunOpts<'static> {
    RunOpts::default()
}

/// Run one isolated agent turn. See module docs for the safety model.
pub async fn run_gray(
    config: &Value,
    config_path: &Path,
    conversation: &str,
    prompt: &str,
    mut opts: RunOpts<'_>,
) -> Result<String, RunError> {
    let timeout_secs = match opts.timeout_secs {
        Some(0) => return Err(RunError::Io("Timeout must be positive".to_string())),
        Some(n) => n,
        None => config
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(600),
    };
    if timeout_secs == 0 {
        return Err(RunError::Io("Timeout must be positive".to_string()));
    }
    if prompt.trim().is_empty() || prompt.chars().count() > 32000 {
        return Err(RunError::Io(
            "Prompt must contain 1–32000 characters".to_string(),
        ));
    }

    let key = hex_sha256(conversation.as_bytes());
    let base = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let home = base.join("conversations").join(&key);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .map_err(|_| RunError::Io("cannot create conversation home".to_string()))?;
    let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));

    // Non-blocking per-conversation lock, held for the whole turn.
    let lock_file = std::fs::File::create(home.join("run.lock"))
        .map_err(|_| RunError::Io("cannot lock conversation".to_string()))?;
    // SAFETY: flock on a file we keep open; fd valid until `_lock` drops.
    use std::os::unix::io::AsRawFd;
    let lock_fd = lock_file.as_raw_fd();
    // Do not let another concurrently spawned gray child inherit this lock
    // descriptor. Otherwise a child for conversation B can keep conversation
    // A's flock alive after A's runner future has returned.
    let fd_flags = unsafe { libc::fcntl(lock_fd, libc::F_GETFD) };
    if fd_flags < 0
        || unsafe { libc::fcntl(lock_fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(RunError::Io("cannot protect conversation lock".to_string()));
    }
    let locked = unsafe { libc::flock(lock_fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return Err(RunError::Busy);
    }
    let _lock = ConversationLock {
        _file: lock_file,
        fd: lock_fd,
    };

    // Snapshot the provider config into the isolated home.
    let gray_home = config
        .get("gray_home")
        .and_then(Value::as_str)
        .unwrap_or("");
    let provider: Value = std::fs::read(Path::new(gray_home).join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| RunError::Io("gray provider configuration is missing".to_string()))?;
    crate::config::atomic_json(&home.join("config.json"), &provider).map_err(RunError::Io)?;

    // Dedicated workdir/profile: no lockfile plugins that might start
    // another gateway.
    let work = home.join("work");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&work)
        .or_else(|e| if work.is_dir() { Ok(()) } else { Err(e) })
        .map_err(|_| RunError::Io("cannot create work directory".to_string()))?;
    let _ = std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700));
    let launcher = home.join("discord-sidecar");
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "gray-discord".to_string());
    let abs_config = config_path
        .to_str()
        .map(str::to_string)
        .unwrap_or_else(|| config_path.to_string_lossy().into_owned());
    let script = format!(
        "#!/bin/sh\nexec {} sidecar --config {}\n",
        shell_word(&exe),
        shell_word(&abs_config)
    );
    std::fs::write(&launcher, script)
        .map_err(|_| RunError::Io("cannot write launcher".to_string()))?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| RunError::Io("cannot protect launcher".to_string()))?;
    let shared = crate::capabilities::prepare(config, &home).map_err(RunError::Io)?;
    let profile = format!(
        "plugins:\n  - builtin: tools-minimal\n  - sidecar: {}\n{}",
        serde_json::to_string(&launcher.to_string_lossy()).unwrap_or_default(),
        shared
    );
    std::fs::write(work.join("gray.yml"), profile)
        .map_err(|_| RunError::Io("cannot write profile".to_string()))?;
    // Bound project discovery at the dedicated work directory.
    let _ = std::fs::create_dir(work.join(".git"));

    let state_path = home.join("session.json");
    let mut state: Value = std::fs::read(&state_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    if !state.is_object() {
        state = Value::Object(Default::default());
    }
    let sessions = home.join("sessions");
    let mut files: Vec<PathBuf> = if sessions.is_dir() {
        std::fs::read_dir(&sessions)
            .map_err(|_| RunError::Io("cannot list sessions".to_string()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect()
    } else {
        Vec::new()
    };
    let now = crate::durable::now_secs();
    let has_session = state
        .get("session_id")
        .and_then(Value::as_str)
        .is_some_and(|sid| !sid.is_empty())
        || files.len() == 1;
    if crate::session::ResetPolicy::from_config(config)
        .reset_reason(&state, has_session, now)
        .is_some()
    {
        crate::session::reset_home(&home, now).map_err(RunError::Io)?;
        state = serde_json::json!({
            "generation": crate::durable::uuid_hex(),
            "last_activity": now
        });
        files.clear();
    }
    if state
        .get("generation")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        state["generation"] = Value::String(crate::durable::uuid_hex());
    }
    state["last_activity"] = serde_json::json!(now);
    crate::config::atomic_json(&state_path, &state).map_err(RunError::Io)?;
    let generation = state
        .get("generation")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if files.len() > 1 {
        return Err(RunError::Protocol(
            "Ambiguous conversation store; refusing to select a session".to_string(),
        ));
    }

    let gray_bin = config.get("gray_bin").and_then(Value::as_str).unwrap_or("");
    if gray_bin.is_empty() {
        return Err(RunError::Io("gray executable is missing".to_string()));
    }
    let max_requests = config
        .get("max_requests")
        .and_then(Value::as_u64)
        .unwrap_or(32);
    let mut args: Vec<String> = vec![
        gray_bin.to_string(),
        "-p".to_string(),
        prompt.to_string(),
        "--json".to_string(),
        "--max-requests".to_string(),
        max_requests.to_string(),
    ];
    let policy = config.get("budget").cloned().unwrap_or(Value::Null);
    let budget_required = config
        .get("budget_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut ledger: Option<Budget> = None;
    let reservation = reservation_id();
    if !policy.is_null() || budget_required {
        let budget = Budget::new(&base.join("budget.sqlite")).map_err(RunError::Io)?;
        let model = provider.get("model").and_then(Value::as_str).unwrap_or("");
        budget
            .reserve(&reservation, &policy, model)
            .map_err(RunError::Budget)?;
        for (flag, key) in [
            ("--max-cost-usd", "turn_usd"),
            ("--input-price", "input_per_million"),
            ("--output-price", "output_per_million"),
        ] {
            args.push(flag.to_string());
            args.push(value_text(policy.get(key).unwrap_or(&Value::Null)));
        }
        ledger = Some(budget);
    }
    let session_id = state
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            files
                .first()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
        });
    if let Some(sid) = &session_id {
        args.push("--session".to_string());
        args.push(sid.clone());
    }
    if let Some(model) = opts.model.as_ref().filter(|m| !m.trim().is_empty()) {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    let mut cmd = tokio::process::Command::new(&args[0]);
    cmd.args(&args[1..]);
    cmd.current_dir(&work);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    // Own process group so a timeout SIGKILL reaches grandchildren too.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    cmd.env_clear();
    for (k, v) in std::env::vars_os() {
        let Some(k_str) = k.to_str() else { continue };
        if k_str.starts_with("GRAY_")
            || k_str.starts_with("DISCORD_")
            || k_str.starts_with("OPENAI_")
        {
            continue;
        }
        cmd.env(k, v);
    }
    cmd.env("GRAY_HOME", &home);
    // A job the model adds with a plain `gray cron add` inherits this
    // conversation's binding, so it comes back to the channel it was added
    // from. Absent outside a chat: the job is just local.
    if let Some(origin) = crate::cron::origin_env(&home) {
        cmd.env("GRAY_CRON_ORIGIN", origin);
    }
    cmd.env("GRAY_SKILLS_ONLY", "1");
    // Tool narration is safe to show; raw model reasoning is not. Keep the
    // wire quiet even when the activity bubble is enabled, matching Hermes.
    cmd.env("GRAY_SHOW_REASONING", "0");
    cmd.env("GRAY_MAX_WALL_SECS", (timeout_secs.max(1)).to_string());
    let mut child = cmd
        .spawn()
        .map_err(|_| RunError::Spawn("spawn failed".to_string()))?;
    let pid = child.id().unwrap_or(0);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RunError::Spawn("no stdout".to_string()))?;
    // Move progress into an owned callback struct so the consume future
    // captures locals — never `opts` itself (receipt is only touched after
    // the await, like the Python's post-turn `receipt.update(final)`).
    let mut progress_cb = ProgressCb {
        call: |ctx, row| {
            if let Some(inner) = ctx.inner.as_deref_mut() {
                inner(row);
            }
        },
        ctx: ProgressCtx {
            inner: opts.progress.take(),
        },
    };
    let consume = async {
        consume_ndjson(
            stdout,
            &state_path,
            &generation,
            &mut state,
            Some(&mut progress_cb),
        )
        .await
    };
    let outcome: Result<Consume, RunError> =
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), consume).await {
            Ok(r) => r,
            Err(_) => {
                // Timeout: kill the whole process group (Python: killpg SIGKILL).
                if pid != 0 {
                    unsafe {
                        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                    }
                }
                let _ = child.wait().await;
                return Err(RunError::Timeout);
            }
        };
    let status = child
        .wait()
        .await
        .map_err(|_| RunError::Spawn("wait failed".to_string()))?;
    let consumed = outcome?;

    if let Some(final_row) = &consumed.terminal {
        if let Some(receipt) = opts.receipt.as_deref_mut() {
            for (k, v) in final_row.as_object().cloned().unwrap_or_default() {
                receipt[k] = v;
            }
        }
        if ledger.is_some() && consumed.protocol_ok {
            let accounting = final_row.get("accounting").cloned().unwrap_or(Value::Null);
            if let Some(budget) = &ledger {
                budget.settle(&reservation, &accounting);
            }
        }
    }
    if !status.success() {
        return Err(RunError::Exit(status.code()));
    }
    if !consumed.protocol_ok {
        return Err(RunError::Protocol(
            "Invalid agent JSON output; upgrade gray to a compatible version".to_string(),
        ));
    }
    let Some(final_row) = consumed.terminal else {
        return Err(RunError::Incomplete);
    };
    if final_row.get("type").and_then(Value::as_str) != Some("result")
        || final_row
            .get("session_id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(RunError::Incomplete);
    }
    let answer = match final_row.get("text").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => Ok(t.to_string()),
        _ => Err(RunError::Empty),
    };
    // Drop the lock before the async state machine is suspended again. This
    // matters when several workers are created in one Tokio process.
    drop(_lock);
    answer
}

struct ConversationLock {
    _file: std::fs::File,
    fd: std::os::unix::io::RawFd,
}

impl Drop for ConversationLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this guard and remains valid
        // until this method returns.
        unsafe {
            libc::flock(self.fd, libc::LOCK_UN);
        }
    }
}

struct Consume {
    terminal: Option<Value>,
    protocol_ok: bool,
}

/// Read NDJSON rows: protocol-1 only, single turn id, session pinning with
/// atomic save on change, terminal result/error capture. Any malformed row
/// sets `protocol_ok = false` but keeps consuming (Python parity: record the
/// protocol error, still wait for the child, still settle on clean rows).
async fn consume_ndjson(
    stdout: tokio::process::ChildStdout,
    state_path: &Path,
    generation: &str,
    state: &mut Value,
    mut progress: Option<&mut ProgressCb<'_>>,
) -> Result<Consume, RunError> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stdout).lines();
    let mut turn_id: Option<String> = None;
    let mut terminal: Option<Value> = None;
    let mut protocol_ok = true;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|_| RunError::Io("cannot read agent output".to_string()))?;
        let Some(line) = line else { break };
        if line.len() > LINE_CAP {
            protocol_ok = false;
            continue;
        }
        let row: Value = match serde_json::from_str(&line) {
            Ok(Value::Object(map)) => Value::Object(map),
            _ => {
                protocol_ok = false;
                continue;
            }
        };
        if row.get("protocol").and_then(Value::as_i64) != Some(1) {
            protocol_ok = false;
            continue;
        }
        let row_turn = row.get("turn_id").and_then(Value::as_str).unwrap_or("");
        if row_turn.is_empty() || (turn_id.is_some() && turn_id.as_deref() != Some(row_turn)) {
            protocol_ok = false;
            continue;
        }
        turn_id = Some(row_turn.to_string());
        // Data after the terminal row is a protocol violation (Python:
        // "Data after terminal agent result"), not a second result.
        if terminal.is_some() {
            protocol_ok = false;
            continue;
        }
        if let Some(sid) = row.get("session_id").and_then(Value::as_str) {
            if uuid_valid(sid) {
                // `/new` advances the on-disk generation while an old child
                // may still be finishing. Never let that child resurrect its
                // predecessor's pointer.
                if !crate::session::generation_is_current(state_path, generation) {
                    protocol_ok = false;
                    continue;
                }
                let pinned = state
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !pinned.is_empty() && pinned != sid {
                    protocol_ok = false;
                    continue;
                }
                state["session_id"] = Value::String(sid.to_string());
                let _ = crate::config::atomic_json(state_path, state);
            } else {
                protocol_ok = false;
                continue;
            }
        }
        match row.get("type").and_then(Value::as_str) {
            Some("result") | Some("error") => {
                terminal = Some(row);
            }
            Some("progress") => {
                if let Some(cb) = progress.as_deref_mut() {
                    (cb.call)(&mut cb.ctx, &row);
                }
            }
            _ => {
                protocol_ok = false;
            }
        }
    }
    Ok(Consume {
        terminal,
        protocol_ok,
    })
}

fn uuid_valid(s: &str) -> bool {
    // Cheap structural check (8-4-4-4-12 lowercase hex); the Python parses
    // with uuid.UUID, which also accepts braces/uppercase — pinned session
    // ids written by gray are always canonical, so strict is fine here.
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        let hex = c.is_ascii_hexdigit();
        let dash = *c == b'-';
        let want_dash = matches!(i, 8 | 13 | 18 | 23);
        if want_dash != dash || (!want_dash && !hex) {
            return false;
        }
    }
    true
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn reservation_id() -> String {
    // 16 hex chars of OS entropy (Python: uuid4 hex, truncated display).
    let mut buf = [0u8; 8];
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut buf);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// Single-quote shell escaping for launcher argv words.
fn shell_word(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
