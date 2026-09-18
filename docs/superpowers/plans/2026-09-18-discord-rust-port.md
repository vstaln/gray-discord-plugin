# gray-discord Rust Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Python `gray_discord` package with a single Rust binary `gray-discord` that reaches the approved Hermes text-gateway parity spec.

**Architecture:** One binary with two modes. `sidecar` speaks the gray protocol-1.1 stdio wire (manifest, `tool/call discord_send`, `prompt/context`). `run` is the owner + `allowed_users` Discord gateway: twilight websocket receive, REST send via reqwest, one `gray -p --json` child per turn with explicit session pointers, SQLite inbox/outbox + budget ledger via rusqlite. No gray-core changes in this repo.

**Tech Stack:** Rust edition 2021, clap 4 (derive), serde/serde_json, tokio (full), rusqlite (bundled), reqwest (rustls-tls + json), rust_decimal, sha2, base64, libc, twilight-gateway + twilight-model (same pinned 0.1x, resolved in Task 1).

**Spec:** `docs/superpowers/specs/2026-09-18-discord-rust-port-design.md`

## Global Constraints

- X-session ban: NEVER run `cargo test` (or any compiled test binary) on this machine while X runs. Local checks only: `cargo fmt --check`, `cargo build`, `cargo clippy -- -D warnings`, all guarded with `nice -n 19 ionice -c3 flock /tmp/cargo.lock`. Tests run in CI.
- Commits are pathspec-limited: `git add <exact paths> && git config core.hooksPath /dev/null && git commit --no-verify -m "<msg>" -- <exact paths>`. NEVER bare `git add -A` / `git commit`.
- No live network, tokens, or provider keys in tests. Discord REST tests use a loopback stub server; runner tests use fixture shell scripts plus `GRAY_TEST_BIN`-gated real-gray tests.
- File modes: new dirs `0700`, `config.json`/SQLite files `0600`, launcher scripts `0700`. Never log tokens or config bodies; user-facing errors print categories, never SDK/config text.
- Wire rules (from `sidecar.py`): frames capped at 256 KiB (exit 1 on overflow), numeric `id` only, notifications (`event/notify`, missing id) ignored, `plugin/shutdown` exits 0, unknown methods reply `{"error": ...}`, manifest answers without config.
- Gray NDJSON contract: `gray -p <prompt> --json --max-requests N [--session SID]`, rows have `protocol: 1`, stdout lines capped at 1 MiB, `session_id` must never switch mid-conversation, nonzero exit always fails the turn.
- Numeric bounds: prompts 1–32000 chars; direct sends 1–20000 chars; terminal answers 1–200000 chars; Discord chunks 2000 UTF-16 units; max 8 chunks per turn with truncation notice; pairing code 18 random bytes, 300 s expiry, constant-time compare; typing refresh 8 s; delivery backoff `min(3600, 2^min(attempts+1,12))` seconds; retention prune 7 days.
- Budget: decimal-ceiling to micro-USD, `0 < turn_usd <= daily_usd`, policy pinned to provider model, unknown/crashed/cancelled usage keeps the reservation.
- Child env: strip `GRAY_*`/`DISCORD_*`/`OPENAI_*`, set `GRAY_HOME`, `GRAY_SKILLS_ONLY=1`, `GRAY_SHOW_REASONING=0`, `GRAY_MAX_WALL_SECS`.
- Service unit stays `gray-discord-plugin.service`: `Type=simple`, `Restart=on-failure`, `RestartSec=10`, `UMask=0077`, `KillMode=control-group`, `TimeoutStopSec=15`, token never in the unit.
- CLI keeps the Python surface verbatim: `gray-discord --config <path> <setup|run|sidecar|register|install|status|stop|restart|doctor|uninstall|limits|budget|share|queue|schedule|allowlist>`, help shows `Usage: gray discord ...`, `--config` precedes the subcommand.

## File Structure

New Rust crate at repo root (Python package stays until Task 15):

- `Cargo.toml`, `Cargo.lock` (committed): `[lib] name = "gray_discord"`, `[[bin]] name = "gray-discord" path = "src/main.rs"`.
- `src/lib.rs`: module declarations only.
- `src/main.rs`: clap dispatch to `cli::run`, controlled-error exit codes.
- `src/cli.rs`: clap `Cli`/`Command` tree (all 16 subcommands incl. new `allowlist`), `register()` (lock.json), subcommand handlers.
- `src/config.rs`: `default_path`, `atomic_json`, `snowflake`, `validate_config`, `load_config`, `save_config`. Ports `config.py`.
- `src/policy.rs`: `incoming()` gate with `allowed_users`, `Pairing`. Ports `policy.py`.
- `src/text.rs`: `utf16_len`, `split_message`. Ports `hermes_text.py`.
- `src/budget.rs`: `BudgetBlocked`, `amount`, `validate`, `Budget` ledger. Ports `budget.py`.
- `src/capabilities.rs`: `absolute`, `prepare`. Ports `capabilities.py`.
- `src/transport.rs`: `Rest` client (send, typing, reactions, slash register/callback/followup, users/@me, applications/@me, channel fetch), `TransportError`. Ports `transport.py` + hermes-rs `discord_tool.rs` constants.
- `src/doctor.rs`: `doctor()` checks. Split out of `cli.py`'s `doctor`.
- `src/sidecar.rs`: `serve()`, `dispatch()`, `MANIFEST`. Ports `sidecar.py`.
- `src/durable.rs`: `Store` (inbox/outbox/schedules/meta + `interaction_token`/`app_id` columns for slash followups). Ports `durable.py`.
- `src/runner.rs`: `run_gray()`, `RunError`. Ports `runner.py`.
- `src/gateway.rs`: `Runtime<R,D>`, `open_store`, `run()`. Ports `gateway.py`.
- `src/service.rs`: `NAME`, `unit_path`, `quote`, `unit`, `control`, `install`, `uninstall`. Ports `service.py`.
- `src/setup.rs`: `invite()`, `setup()` wizard with injectable prompter. Ports `setup.py`.
- `tests/common/mod.rs`: loopback HTTP stub, temp fixture builders.
- `tests/*.rs`: `budget`, `capabilities`, `core` (config+text+policy), `delivery`, `durable`, `package`, `runner`, `runtime`, `schedule`, `setup_cli` — one per Python test file.
- `.github/workflows/test.yml`: add `rust` job (fmt, clippy `-D warnings`, test, all single-thread).
- `.github/workflows/release.yml`: new, copied from `gray-background` matrix with asset prefix `gray-discord-`.

---

### Task 1: Scaffold crate, full CLI tree, CI job

**Files:**
- Create: `Cargo.toml`, `Cargo.lock` (via build), `src/lib.rs`, `src/main.rs`, `src/cli.rs`
- Modify: `.gitignore` (add `/target/`), `.github/workflows/test.yml` (add `rust` job)
- Test: `tests/package.rs` (help output only for now; more cases land in Task 6)

**Interfaces:**
- Consumes: nothing (first task).
- Produces:
  - `cli::Cli { config: Option<PathBuf>, command: Command }` — clap parser, `name = "gray-discord"`.
  - `cli::Command` — all 16 variants: `Setup, Run, Sidecar, Register, Install, Status, Stop, Restart, Doctor, Uninstall, Limits { timeout_seconds, concurrency, max_requests }, Budget { action }, Share { skill, context, plugin_argv, clear }, Queue { action }, Schedule { action }, Allowlist { action }`, each with the exact flags of `cli.py parser()`.
  - `cli::run(cmd: &Command, config_path: &Path) -> Result<(), String>` — stub returning `Err("not yet implemented")` except `--help` handling (clap does that).
  - `config::default_path() -> PathBuf` — minimal stub (full module is Task 2); Task 1 needs it only to resolve the config path.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "gray-discord"
version = "0.1.0"
edition = "2021"
rust-version = "1.89"
license = "MIT"
description = "Standalone Hermes-inspired Discord transport and setup for gray"

[lib]
name = "gray_discord"
path = "src/lib.rs"

[[bin]]
name = "gray-discord"
path = "src/main.rs"

[dependencies]
anyhow = "1"
base64 = "0.22"
clap = { version = "4", features = ["derive"] }
libc = "0.2"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
rusqlite = { version = "0.32", features = ["bundled"] }
rust_decimal = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
tokio = { version = "1", features = ["full"] }
twilight-gateway = "0.17"
twilight-model = "0.17"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile = "3"
```

Before writing, resolve the twilight version: run `cargo search twilight-gateway | head -3` (network is available). If the latest 0.x differs from `0.17`, use the latest `0.x` for BOTH twilight crates and record the chosen version in the commit message. (The gray-history salvage used 0.17; anything newer in the 0.x line keeps the same API shape. If crates.io shows 1.x, stop and ask — the event API changed.)

- [ ] **Step 2: Write `src/lib.rs`, `src/main.rs`, `src/cli.rs` skeleton**

```rust
// src/lib.rs
pub mod budget;
pub mod capabilities;
pub mod cli;
pub mod config;
pub mod doctor;
pub mod durable;
pub mod gateway;
pub mod policy;
pub mod runner;
pub mod service;
pub mod setup;
pub mod sidecar;
pub mod text;
pub mod transport;
```

```rust
// src/main.rs
fn main() {
    use clap::Parser;
    let cli = gray_discord::cli::Cli::parse();
    let path = cli.config_path();
    match gray_discord::cli::run(&cli.command, &path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
```

`src/cli.rs` defines the full clap tree (all flags/options exactly as in `gray_discord/cli.py parser()`), `config_path()` (explicit `--config` or `config::default_path()`), and `run()` stubbed to `Err("not yet implemented".into())` for every variant. The `Budget/Queue/Schedule/Allowlist` sub-actions are nested enums (`BudgetAction::Status/Set{...}`, etc.) with the exact Python flags (`--daily-usd` etc. as `f64`, `--every` as `u64`, `--timeout-seconds` etc. as `Option<u64>`).

- [ ] **Step 3: Write the failing test** (`tests/package.rs`, help case only)

```rust
use std::process::Command;

#[test]
fn help_lists_subcommands() {
    let out = Command::new(env!("CARGO_BIN_EXE_gray-discord"))
        .arg("--help")
        .output()
        .expect("spawn");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    for cmd in ["setup", "run", "sidecar", "register", "doctor", "schedule", "allowlist"] {
        assert!(text.contains(cmd), "help missing {cmd}");
    }
}
```

(If the compiler rejects `CARGO_BIN_EXE_gray-discord` because of the hyphen, use `CARGO_BIN_EXE_gray_discord` — keep whichever compiles; do not rename the binary: the installed executable must stay `gray-discord` for gray's `gray-{name}` PATH discovery.)

- [ ] **Step 4: Verify** — local (X ban: build only): `nice -n 19 ionice -c3 flock /tmp/cargo.lock cargo build --locked` must succeed. Full `cargo test --locked -- --test-threads=1` runs in CI. Append the `rust` job to `.github/workflows/test.yml` (fmt --check, clippy `-D warnings`, test single-thread) and `/target/` to `.gitignore` in this same task.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/main.rs src/cli.rs tests/package.rs .gitignore .github/workflows/test.yml
git config core.hooksPath /dev/null
git commit --no-verify -m "feat(discord): scaffold Rust crate with full CLI tree" -- Cargo.toml Cargo.lock src/lib.rs src/main.rs src/cli.rs tests/package.rs .gitignore .github/workflows/test.yml
```

### Task 2: config + text + policy (pure logic, ports `config.py`, `hermes_text.py`, `policy.py`)

**Files:**
- Create: `src/config.rs`, `src/text.rs`, `src/policy.rs`
- Test: `tests/core.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `config::default_path() -> PathBuf` (`$HOME/.config/gray-discord/config.json`).
  - `config::atomic_json(path: &Path, data: &serde_json::Value) -> Result<(), String>` (parents `0700`, temp+rename, pretty + trailing newline, replacement file `0600`).
  - `config::snowflake(v: &serde_json::Value) -> bool` (ASCII digits, `0 < n < 2^64`).
  - `config::validate_config(data: &serde_json::Value) -> Result<(), String>` — object check; non-blank `token`; `owner_id`/`channel_id` snowflakes; `allowed_users` if present must be an array of snowflakes (NEW vs Python); `gray_bin`/`gray_home`/`workdir` absolute; int ranges `timeout_seconds` 1–86400, `concurrency` 1–16, `max_requests` 1–1000, `queue_capacity` 1–100000 (only when present); `budget` validated via `budget::validate` (Task 4 — call it; Task 4 lands before this compiles).
  - `config::load_config(path) -> Result<serde_json::Value, String>`, `config::save_config(path, &Value) -> Result<(), String>` (error text exactly `Configuration missing or invalid; run setup` / `Bot token is missing; run setup` / `<key> must be a Discord ID|an absolute path|an integer between <low> and <high>`). Never include config values in errors.
  - `text::utf16_len(s: &str) -> usize` (`s.encode_utf16().count()`), `text::split_message(s: &str, limit: usize) -> Result<Vec<String>, String>` (limit >= 2, longest UTF-16-safe prefix loop, error `Message limit must be at least two UTF-16 units`). Default limit 2000 — pass explicitly at call sites.
  - `policy::incoming(author: &str, owner: &str, bot: bool, dm: bool, text: &str, bot_id: &str, allowed: &[String]) -> Option<String>` — None if `owner.is_empty()`, `author != owner && !allowed.contains(author)`, or `bot`; non-DM requires `<@bot_id>` or `<@!bot_id>` mention; strip mentions, trim, None on empty. (Python took only `owner`; the `allowed` param is the §2 addition.)
  - `policy::Pairing { code: String, expires: f64, used: bool }`, `Pairing::new(now: f64) -> Self` (code = 18 random bytes via OS RNG, base64url-nopad like `secrets.token_urlsafe(18)`), `accept(&mut self, text: &str, now: f64) -> bool` (constant-time compare, single-use, `now >= expires` fails).

- [ ] **Step 1: Write failing tests** (`tests/core.rs`) — port `tests/test_core.py` verbatim plus allowlist cases:

```rust
use gray_discord::{config, policy, text};
use serde_json::json;
use std::path::PathBuf;

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
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(config::validate_config(&json!({})).is_err());
    std::fs::write(&path, "{bad").unwrap();
    assert!(config::load_config(&path).unwrap_err().contains("run setup"));
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
    assert_eq!(policy::incoming("123", "123", false, true, "hello", "456", &[]), Some("hello".into()));
    assert_eq!(policy::incoming("123", "123", false, false, "<@!456> hello", "456", &[]), Some("hello".into()));
    assert_eq!(policy::incoming("777", "123", false, true, "hi", "456", &allowed), Some("hi".into()));
    for (a, b, dm, t) in [("999", false, true, "x"), ("123", true, true, "x"),
                          ("123", false, false, "hello"), ("777", false, false, "hello")] {
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
```

- [ ] **Step 2: Implement** the three modules. Pairing RNG: read 18 bytes from `/dev/urandom` (or `getrandom` via `libc::getentropy` — no new deps; fall back to hashing pid+time ONLY if the RNG read fails, and note it in code). Constant-time compare: hand-rolled byte loop with `std::hint::black_box`, no `==` early exit.
- [ ] **Step 3: Verify** — `cargo build --locked` locally (X ban); CI runs `cargo test --locked -- --test-threads=1` with all 4 tests green.
- [ ] **Step 4: Commit** (pathspec-limited, `--no-verify`, message `feat(discord): port config, text splitting, admission gate`).

### Task 3: durable queue + budget ledger (ports `durable.py`, `budget.py`)

**Files:**
- Create: `src/durable.rs`, `src/budget.rs`
- Test: `tests/durable.rs`, `tests/budget.rs`

**Interfaces:**
- Consumes: `config::atomic_json` NOT used here (SQLite paths); `text::split_message` for `complete()`.
- Produces:
  - `budget::BudgetBlocked(String)` error type; `budget::amount(v: &Value, scale: i64) -> Result<i64, BudgetBlocked>` (decimal-ceiling to int via `rust_decimal`, finite + non-negative else `Budget and prices must be finite nonnegative numbers`); `budget::validate(policy: &Value, model: &str) -> Result<(i64, i64), BudgetBlocked>` (model pin, both per-million prices, `0 < turn <= daily` else the exact Python messages); `budget::Budget { fn new(path: &Path) -> Result<Self, String> }` creating parents `0700`, file `0600`, `reservations` table; methods `reserve(&self, id, policy, model) -> Result<i64, BudgetBlocked>` (UTC-day bucket, replay + daily-cap checks with `BEGIN IMMEDIATE`), `settle(&self, id, accounting: &Value)` (only when `usage_complete == true` and `cost_micros` is a non-negative int, else keep), `total(&self) -> i64`.
  - `durable::Store { fn new(path: &Path) -> Result<Self, String> }` creating parents `0700`, file `0600`, the four Python tables verbatim PLUS `ALTER TABLE inbox ADD COLUMN` for `interaction_token TEXT` and `ADD COLUMN app_id TEXT` (needed by Task 9 slash followups; idempotent `match` on duplicate-column errors so old DBs migrate). PRAGMA `synchronous=FULL`, `busy_timeout` 10 s, every method wraps in `BEGIN IMMEDIATE`.
  - Methods mirroring Python exactly: `enqueue(id, channel, prompt, conversation: Option<&str>, capacity) -> Result<bool, String>` (dedup → `Ok(false)`; capacity error `Queue is full; message was not accepted`); `claim() -> Option<InboxItem>` (per-conversation serialization); `get(id)`, `items()` (last 100 `id,channel,state,error`); `cancel(id)` (`No queued/running item with that ID`); `complete(id, text, receipt: &Value)` (1–200000 chars, chunks via `text::split_message`); `fail(id, code)` (map unknown codes to `agent_failed`, insert the controlled `Turn {id}: {code}...` notice); `recover()`; `next_delivery(now)`, `ack(id, part, message_id)`, `delivery_failed(part, code, now)` (same backoff formula); `schedule_add/remove/schedules/enqueue_due/migrate_jobs`; NEW `prune_terminal(older_than_secs) -> usize` (delete `sent`/`cancelled`/`uncertain` inbox rows older than 7 days — Hermes `recovery.py` parity; outbox parts for those rows go too).

- [ ] **Step 1: Write failing tests** — port `tests/test_budget.py` (all 3 tests) to `tests/budget.rs` and `tests/test_durable.py` (all 4 tests) to `tests/durable.rs`, plus for prune: enqueue → complete → ack → backdate `created` → `prune_terminal` removes; unsettled ledger rows are never pruned.
- [ ] **Step 2: Implement** both modules. Decimal math: parse via `rust_decimal::Decimal::from_str(&v.to_string())`, ceiling with `.ceil()`, reject non-finite (NaN/Inf parse or compare failures → `BudgetBlocked`). UTC day: `chrono::Utc::now().date_naive()`.
- [ ] **Step 3: Verify** — build locally; CI runs both test files green.
- [ ] **Step 4: Commit** (`feat(discord): port durable queue and budget ledger`).

### Task 4: Discord REST transport + doctor (ports `transport.py`, `cli.py doctor`, hermes-rs `discord_tool.rs`)

**Files:**
- Create: `src/transport.rs`, `src/doctor.rs`
- Test: `tests/delivery.rs` (extend in later tasks only by addition)

**Interfaces:**
- Consumes: `text::split_message`, `config` values.
- Produces:
  - `transport::TransportError` enum: `Auth(String)`, `Forbidden(String)`, `RateLimited(Option<f64>)`, `Http(u16, String)`, `Net(String)`, `Invalid(String)` — all `Display` impls print categories + status only, never bodies beyond 200 chars, never tokens. `is_retryable(&self) -> bool` (429/5xx true).
  - `transport::Rest { fn new(base: &str, token: &str) -> Self }` — `base` defaults to `https://discord.com/api/v10`; tests inject the loopback stub. reqwest client with rustls, `Authorization: Bot <token>`, `Content-Type: application/json`. NEVER read a `base` from production config (the Python comment `Production code has no api_base setting` is a security rule: only tests/fixtures pass non-default bases — enforce by making `base` a constructor arg the CLI never sets from config).
  - Methods: `login(&self) -> Result<UserId, TransportError>` (`GET /users/@me`); `fetch_channel(&self, id: u64) -> Result<Channel, TransportError>`; `send(&self, channel: u64, text: &str, nonce: Option<&str>) -> Result<MessageId, TransportError>` (validate 1–20000 chars first, split via `text::split_message`, `allowed_mentions: {"parse": []}`, no `message_reference`, one POST per chunk in order, return first message id — port `send()`); `typing(&self, channel: u64)` (POST typing, ignore errors — progress path must not fail turns); `add_reaction/remove_reaction(&self, channel, message, emoji: &str)` (PUT/DELETE, ignore errors); `register_commands(&self, app_id: &str, cmds: &Value)` (bulk overwrite PUT); `interaction_callback(&self, id, token, data: &Value)` (POST deferred-channel-message-with-source); `followup(&self, app_id, token, text chunks)` (POST webhook followups, also chunked); `rest_send(&self, channel_id: &str, text: &str)` (login → fetch → send, port of `rest_send`); rate-limit: on 429 read `retry-after` header (float secs) and return `RateLimited`, caller backs off (delivery loop, Task 9).
  - Hermes constants ported in `transport.rs`: `MAX_CONTENT_LEN = 2000`, `MAX_EMBEDS = 10`, `INVITE_PERMISSIONS = 68608`, gateway intents bits for later (`1<<9 | 1<<15 | 1<<18` message-content privileged, DMs need no members intent).
  - `doctor::doctor(config: &Value) -> Result<(), String>`: gray_bin executable check; provider `config.json` has `model`; `Rest::login`; `GET /applications/@me` message-content-intent flag check; fetch home channel, and if guild channel check bot member has view/send/history (via the channel + guild-member endpoints). Success prints both Python lines verbatim (`Bot token, intent, ... verified.` + `Provider generation and ... were not tested.`).

- [ ] **Step 1: Write failing tests** (`tests/delivery.rs` + `tests/common/mod.rs`): loopback stub (`tokio::net::TcpListener`, routes for `/users/@me`, `/applications/@me`, `/channels/42`, `/channels/42/messages` POST recording auth header + body, toggleable 401/403) asserting: emoji text splits into 2 chunks joined == input, auth header is `Bot TESTTOKEN`, `allowed_mentions.parse == []`, no `replied_user`, no `message_reference`, 401→`Auth` with zero sends, 403→`Forbidden`, pre-HTTP rejection for `""`/`20001`-char/`None`.
- [ ] **Step 2: Implement** both modules.
- [ ] **Step 3: Verify** — build locally; CI green.
- [ ] **Step 4: Commit** (`feat(discord): port Discord REST transport and doctor`).

### Task 5: runner — isolated `gray -p --json` child (ports `runner.py`)

**Files:**
- Create: `src/runner.rs`
- Test: `tests/runner.rs`

**Interfaces:**
- Consumes: `config::atomic_json`, `budget::Budget`, `capabilities::prepare` (Task 7 — signature `prepare(config: &Value, home: &Path) -> Result<String, String>`; Task 5 codes against it).
- Produces:
  - `runner::RunError` enum: `Busy`, `Budget(BudgetBlocked)`, `Timeout`, `Spawn(String)`, `Protocol`, `Exit(Option<i32>)`, `Incomplete`, `Empty`, `Io(String)` — `Display` uses the exact Python messages (`This conversation is busy`, `Agent did not return a completed result; actions may already have occurred`, `Agent returned no final answer`, `Invalid agent JSON output; upgrade gray to a compatible version`, `gray exited with code {n}; not retrying possible side effects`). No stdout/stderr bodies in errors.
  - `runner::run_gray(config: &Value, config_path: &Path, conversation: &str, prompt: &str, opts: RunOpts) -> Result<String, RunError>` with `RunOpts { timeout_secs: Option<u64>, progress: Option<Box<dyn FnMut(&str) + Send>>, receipt: Option<&mut serde_json::Value> }`.
  - Behavior, in order: timeout>0 check (`Timeout must be positive`); prompt 1–32000; `conversations/<sha256(conversation)>` home `0700` + non-blocking `run.lock` (`Busy` if locked — use `libc::flock` with `LOCK_EX|LOCK_NB` on a `File::create`d handle held for the whole turn); snapshot provider `config.json` into home; `work/` dir + `.git/` marker; launcher script at `home/discord-sidecar` (`0700`, `#!/bin/sh\nexec '<current-exe>' sidecar --config '<abs path>'` — single-quote shell-escaped; resolves `std::env::current_exe` at runtime, NOT `sys.executable`); `capabilities::prepare` output appended to `work/gray.yml` (`tools-minimal` + sidecar + shared); legacy single-`.jsonl` resume else `session.json`; budget reserve when policy present or `budget_required` (append `--max-cost-usd/--input-price/--output-price`); spawn with `tokio::process::Command`, `stdin null`, `stdout piped`, `stderr null`, `kill_on_drop(true)`, own process group (`pre_exec(|| { libc::setpgid(0,0); Ok(()) })`), filtered env + the four `GRAY_*` overrides; NDJSON consume with 1 MiB line cap, single-`turn_id` enforcement, session-id pinning (atomic save on change), terminal `result`/`error` capture; timeout kills the process group (`libc::kill(-pid, SIGKILL)`); settle ledger on clean terminal rows; `receipt.update(final)` whenever `final` exists.

- [ ] **Step 1: Write failing tests** (`tests/runner.rs`): fake-`gray` shell scripts — nonzero-exit-with-stdout (`code 7`), sleep-then-timeout twice (lock released between), NDJSON session-switch rejection, multi-terminal-row rejection, busy-lock contention. `GRAY_TEST_BIN` real-gray tests (resume replays history, conversations isolated, terminal NDJSON only — no `final_reply` transcript scan): `#[ignore]` unless env set; CI sets it to the `gray` on PATH.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — build locally; CI green (`GRAY_TEST_BIN=$(which gray)` in the workflow env if gray is installed there, else skipped).
- [ ] **Step 4: Commit** (`feat(discord): port isolated gray runner`).

### Task 6: sidecar stdio server + CLI register/package (ports `sidecar.py`, `cli.py register`, `service.rs` unit fns)

**Files:**
- Create: `src/sidecar.rs`
- Modify: `src/cli.rs` (wire `register`, `sidecar` handlers; other subcommands still stubbed)
- Test: extend `tests/package.rs` (keep Task 1 help test, append below)

**Interfaces:**
- Consumes: `config::load_config`, `transport::Rest::rest_send` (20 s timeout via `tokio::time::timeout`).
- Produces:
  - `sidecar::MANIFEST: serde_json::Value` — `discord` 0.1.0, protocol `1.1`, `commands: []`, `hooks: ["prompt/context"]`, `discord_send` tool (description + `{content: string}` required, verbatim from `sidecar.py`).
  - `sidecar::dispatch(method: &str, params: &Value, config_path: &Path) -> Value` — `plugin/manifest` (no config read), `prompt/context` (exact text `discord_send sends to your configured home channel; never send secrets.`), `tool/call` (unknown tool → `is_error`; `rest_send` failure → `Discord delivery failed. Run gray-discord doctor; do not blindly retry partial sends.` with `is_error: true`, config text never echoed), else `{"error": "Unsupported method"}`.
  - `sidecar::serve(config_path: &Path) -> !` — blocking stdin loop: 256 KiB+1 reads (overflow → exit 1), blank/non-JSON/non-object lines skipped, `plugin/shutdown` exits 0, non-numeric `id` ignored, non-object `params` → `{}`, one JSON reply per line + flush.
  - `cli::register(config: &Value, config_path: &Path) -> Result<(), String>`: read `plugins/lock.json` under `gray_home` (missing → fresh `schema: 1`), schema/ shape checks, refuse to overwrite a differing existing `discord` argv (`Another discord plugin is registered; refusing to overwrite it`), insert `ecosystem gray-native / version 0.1.0 / source https://github.com/vstaln/gray-discord-plugin / argv [<current-exe>, sidecar, --config, <abs>] / adapter_version 1.1 / installed_at epoch / scope user / enabled true` via `config::atomic_json`.
  - `service::quote`, `service::unit`, `service::NAME` (needed by package tests; full service module is Task 11).

- [ ] **Step 1: Append failing tests** to `tests/package.rs`: manifest-over-stdio (spawn binary `sidecar --config <missing>`, feed `plugin/manifest` + unknown `tool/call`, assert rows), delivery-failure secrecy (unit-test `dispatch` with a config whose token is `PRIVATE-TOKEN-<random>`, assert `is_error` and absence of the token in the JSON), service unit escaping (`%`/spaces, `KillMode=control-group`, newline rejected), registration preserves other plugins + double-register idempotent + conflicting path refused.
- [ ] **Step 2: Implement** `sidecar.rs`, `service.rs` (`quote`/`unit`/`NAME` fully, plus `control`/`install`/`uninstall` stubs returning `Err("not yet implemented")` — Task 11 fills them), wire `cli.rs` `Sidecar` + `Register` arms.
- [ ] **Step 3: Verify** — build locally; CI green.
- [ ] **Step 4: Commit** (`feat(discord): port sidecar server and registration`).

### Task 7: capabilities (ports `capabilities.py`)

**Files:**
- Create: `src/capabilities.rs`
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `capabilities::absolute(v: &str) -> Result<PathBuf, String>` (absolute + exists, else `Shared capability paths must be absolute and exist`); `capabilities::prepare(config: &Value, home: &Path) -> Result<String, String>` — `skills/` dir `0700`, desired set keyed by hex sha256 of the path string, remove stale symlinks, refuse non-symlink collisions (`Shared skill path collision; refusing to overwrite`), require `SKILL.md` (`Shared skill must be a directory containing SKILL.md`); context files capped at 128 KiB total (`Shared context exceeds 128 KiB; select smaller files`), UTF-8 required, write `AGENTS.md` + `shared-context.json` marker (remove both when empty); `shared_plugins` argv arrays validated (nonempty, all strings, no NUL, nonempty exe → `Shared plugins require nonempty argv arrays` / `Shared plugin executable is empty`), launchers `shared-plugin-{i}` mode `0700` with single-quote shell escaping, profile lines `  - sidecar: <json-string>\n`.

- [ ] **Step 1: Write failing tests** — port `tests/test_capabilities.py` verbatim (skills+context+plugins round-trip, removal on empty, reject relative/empty/missing/oversize).
- [ ] **Step 2: Implement.** This unblocks Task 5's `prepare` import — if Task 5 landed first with a local stub, delete the stub in this task.
- [ ] **Step 3: Verify** — build locally; CI green.
- [ ] **Step 4: Commit** (`feat(discord): port capability sharing`).

### Task 8: setup wizard (ports `setup.py`)

**Files:**
- Create: `src/setup.rs`
- Test: `tests/setup_cli.rs`

**Interfaces:**
- Consumes: `config::save_config/snowflake`, `policy::Pairing`, `transport::Rest`, `cli::register`, `service::install` (Task 11 — code against `service::install(path: &Path) -> Result<(), String>`).
- Produces: `setup::invite(app_id: &str) -> String` (exact URL `https://discord.com/oauth2/authorize?client_id={id}&scope=bot&permissions=68608`); `setup::run(path: &Path, io: &mut dyn Prompter) -> Result<bool, String>` where `Prompter` has `prompt(&mut self, text: &str) -> Result<String, String>` (visible input), `prompt_hidden(&mut self, text: &str) -> Result<String, String>` (no-echo token), `confirm(&mut self, text: &str) -> Result<bool, String>`. Flow in Python order: TTY check (`Setup needs a terminal for hidden token input` — real prompter reports non-TTY; tests use a fake), existing-config replace confirm (decline → `Ok(false)`), gray binary resolve (`GRAY_BIN` env or `PATH` lookup, else `Install gray first; executable not found`), gray home + provider model check (`Configure a model in gray before setup`), budget prompts + `budget::validate`, hidden token, `Rest::login`, print invite + intent instructions, wait-for-enter, pairing code print, 300 s wait for owner DM (poll gateway events — full wiring in Task 9; here accept an injected `wait_for_pairing: &dyn Fn(&str) -> Result<(String, String), String>` param), owner-ID confirm (decline → `Pairing not confirmed; nothing saved`), home-channel default-to-DM + snowflake check (`Invalid channel ID`), `save_config` with the exact Python field set, `Configuration saved privately.`, `Ok(true)`. CLI `setup` arm: on `Ok(true)` call `register`, print `Outgoing tool registered with gray.`, ask service install (`Enable and start the background service now? [Y/n] `, default yes).

- [ ] **Step 1: Write failing tests** — port `tests/test_setup_cli.py` via the fake `Prompter` (register+install on yes/empty, neither on cancel), plus `setup::run` accept/decline paths with an injected pairing fn.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — build locally; CI green.
- [ ] **Step 4: Commit** (`feat(discord): port setup wizard`).

### Task 9: gateway runtime — queue workers, twilight receive, slash, delivery (ports `gateway.py`)

**Files:**
- Create: `src/gateway.rs` (modify `tests/delivery.rs` NOT at all)
- Test: `tests/runtime.rs`, plus slash admission cases in `tests/core.rs` (append, do not rewrite)

**Interfaces:**
- Consumes: everything above; `policy::incoming`, `runner::run_gray`, `durable::Store`, `budget::BudgetBlocked`, `transport::Rest`, `text::split_message`.
- Produces:
  - `gateway::open_store(config_path: &Path) -> Result<Store, String>` (queue.sqlite + `migrate_jobs(jobs.json)`).
  - `gateway::Runtime<R, D>` generic over `runner: Fn(...) -> Future<Output=Result<String, RunError>>` and `deliver: Fn(OutboxPart) -> Future<Output=Result<String, String>>`, with `generate_one()` (claim → run with progress callback that swallows UI errors → `complete` / `fail` mapping `Budget→budget_blocked`, `Timeout→timeout`, `Cancelled→cancelled`, else `agent_failed`; cancellation polled like the Python 0.25 s loop) and `deliver_one()` (empty id → fail, else `ack` / `delivery_failed`). Constructor takes `(config, config_path, store, deliver, runner)`.
  - `gateway::run(config_path: &Path) -> Result<(), String>`: parents `0700`, `gateway.lock` non-blocking flock (`Another gateway is running`), budget/model validation then force `budget_required`, twilight shard with intents guild-messages + DMs + message-content, `on_ready` print (`Discord connected; durable owner-only queue enabled.`), register the 4 slash commands (bulk overwrite with the stored `app_id`; skip + warn if unchanged — Hermes `_safe_sync_slash_commands` shape), message handler (admission → `enqueue` with stable Discord message-id, full/ invalid → busy reply text verbatim), slash handler (`/ask` defers via `interaction_callback` then enqueues with `(token, app_id)` stored on the row; `/reset` clears that user's session pointer file; `/status` replies ephemerally with queue depth; `/stop` sets `cancel` on that conversation's running row), delivery worker (fetch channel → `Rest::send` per ordered part with stable nonce `sha256("{id}:{part}")[:24]` → `ack`; slash-originated rows use `followup` instead), `N = concurrency` generation workers + 1 delivery + 1 schedule ticker (`enqueue_due(home)` every 1 s), any worker death stops the process (nonzero exit → systemd restarts), SIGTERM cancels cleanly.
  - Slash command definitions: `ask` (+required string `prompt`), `reset`, `status`, `stop` — descriptions from the gray-history salvage.

- [ ] **Step 1: Write failing tests** (`tests/runtime.rs`): port `tests/test_runtime.py` both tests against `Runtime` with fake runner/deliver (no Discord), plus slash-originated followup routing (row with `interaction_token` → deliver fn receives followup marker, not channel send).
- [ ] **Step 2: Implement.** Twilight receive: `twilight_gateway::Shard` event loop deserializing `MessageCreate` + `InteractionCreate`; mention/role data read from the event payloads.
- [ ] **Step 3: Verify** — build locally; CI green (the `GRAY_TEST_BIN` background test runs here too if gray is on CI PATH).
- [ ] **Step 4: Commit** (`feat(discord): port gateway runtime`).

### Task 10: reactions + typing cadence + chunk-cap notice (Hermes Phase-1 behaviors)

**Files:**
- Modify: `src/gateway.rs`, `src/transport.rs`
- Test: extend `tests/runtime.rs` + `tests/delivery.rs` (append only)

**Interfaces:**
- Consumes: Task 9 runtime.
- Produces: on claim → `add_reaction(channel, message, "👀")` (best-effort, never fails the turn); on terminal state → remove 👀 then `✅` (sent) / `❌` (uncertain/cancelled/budget_blocked) — `adapter.py:3356-3383`; typing POST at most every 8 s per channel during `generate_one` (progress callback, errors swallowed); `complete()` caps at 8 chunks — chunks beyond 8 are replaced by a single notice chunk (`... (truncated, N more characters not sent)`) — `MAX_SPLIT_MESSAGES = 8`. Gated on a `reactions: bool` config flag defaulting true.

- [ ] **Step 1: Append failing tests** — reaction call sequence on success/failure (fake deliver transport recording calls), typing cadence (fake clock: progress ticks at t=0/3/9 → 2 typing calls), 9-chunk answer collapses to 8 + notice.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — build locally; CI green.
- [ ] **Step 4: Commit** (`feat(discord): Hermes reactions, typing, chunk cap`).

### Task 11: service unit + remaining CLI arms (ports `service.py`, `cli.py` handlers)

**Files:**
- Modify: `src/service.rs` (complete), `src/cli.rs` (all remaining arms)
- Test: `tests/schedule.rs`, extend `tests/setup_cli.rs` (CLI-level allowlist/schedule/limits/budget/share/queue cases)

**Interfaces:**
- Consumes: `gateway::open_store`, `budget::Budget`, `capabilities::prepare`, `service::*`, `setup::run`, `doctor::doctor`.
- Produces: `service::control(args: &[String]) -> Result<(), String>`, `service::install(path: &Path) -> Result<(), String>`, `service::uninstall() -> Result<(), String>` (exact Python semantics incl. `A different plugin service already exists; uninstall it first`); `service::unit(config_path)` with `ExecStart=<current-exe> run --config <quoted>` (Rust binary replaces `sys.executable -m gray_discord`); CLI arms: `share` (clear/append/dedupe/validate-then-save, `Shared capabilities saved. Only select trusted code/non-secret context. Restart to apply.`), `limits` (`Limits saved; restart the gateway to apply.`), `budget set|status`, `register` (`Outgoing tool registered. Restart existing gray sessions to load it.`), `install` (linger hint), `doctor` (30 s timeout), `run` (SIGTERM → cancel), `schedule add|list|remove` (list prints `{id} {interval} {status}\n` per job), `queue list|cancel`, `allowlist add|remove|list` (snowflake-validated, save via `save_config`; list prints one ID per line), controlled-error envelope (ValueError-ish → message to stderr; anything else → `{kind}: operation failed. Check configuration/connectivity; credentials withheld.`, exit 1).

- [ ] **Step 1: Write failing tests** — port `tests/test_schedule.py` (include the gateway-lock-held online CRUD), CLI allowlist add/list/remove round-trip via subprocess, error-envelope secrecy (bad config → exit 1, no `Traceback`, no token).
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify** — build locally; CI green.
- [ ] **Step 4: Commit** (`feat(discord): complete service lifecycle and CLI`).

### Task 12: release workflow + README cutover docs

**Files:**
- Create: `.github/workflows/release.yml`
- Modify: `README.md`

**Interfaces:**
- Consumes: the built binary (manifest asserts `name == "discord"`, `version == 0.1.0`, `protocol == "1.1"`).
- Produces: release workflow copied from `gray-background` (same 4-target matrix, tag check, musl linker, `cargo test` gating) with asset prefix `gray-discord-` and binary name `gray-discord`; README gains a `Rust binary (0.2.0)` section — `gray install plugin discord`, prebuilt targets, config reuse/migration note (`allowed_users` defaults `[]`), Python deprecation note (still shipped until Task 15).

- [ ] **Step 1: Write the workflow** (no test code; verify by `actionlint` if available, else careful diff vs background's file).
- [ ] **Step 2: Verify** — `git diff --no-index` review against background's `release.yml`; local `cargo build --locked` still green.
- [ ] **Step 3: Commit** (`chore(discord): release workflow and README`).

### Task 13: gray-core CATALOG pin (separate repo, listed for ordering)

**Files (in `/home/vstaln/gray`, NOT this repo):**
- Modify: `crates/gray/src/plugin_cli.rs` — discord CATALOG entry from the Python git pin to the new release asset URL.

**Interfaces:**
- Consumes: a published `v0.2.0` release of this repo with all four assets present.

- [ ] **Step 1: After tagging `v0.2.0` here and confirming assets**, bump the pin in gray, run gray's plugin-install tests, commit there.
- [ ] **No code in this repo for this task** — it exists so executors do it in order.

### Task 14: conformance audit — every Python behavior accounted for

**Files:**
- Create: `docs/port/CONFORMANCE.md` (or extend per-task notes if the worker kept them)

**Interfaces:**
- Consumes: all ported modules + Hermes adapter sections cited in the spec.

- [ ] **Step 1: Build the behavior table** — one row per Python function/branch (`policy.incoming` mention forms, `runner` NDJSON states, `durable` state machine, `gateway` workers, `setup` prompts, every CLI arm) → Rust location → PORTED/DIVERGED/DEFERRED. Every Hermes Phase-1 item from the spec gets a row too.
- [ ] **Step 2: Fix any PORTED-claimed-but-missing rows** with follow-up commits (or re-mark honestly as DEFERRED with reason).
- [ ] **Step 3: Commit** (`docs(discord): conformance audit`).

### Task 15: delete the Python package

**Files:**
- Delete: `gray_discord/`, `tests/*.py`, `pyproject.toml`, Python CI job, `gray_discord_plugin.egg-info/`, `build/`, `dist/`, `.venv/`
- Modify: `README.md` (remove Python install/dev sections), `.gitignore` (drop Python entries already covered), `THIRD_PARTY_NOTICES.md` (keep Hermes attribution — the algorithms survive — reword the packaging sentence)

**Interfaces:**
- Consumes: green CI on the Rust suite + a successful `gray install plugin discord` from a release asset (Task 13 proves it).

- [ ] **Step 1: Verify preconditions** — CI green on `main`, release asset installs, existing `~/.config/gray-discord/config.json` loads (add `allowed_users: []` default on save).
- [ ] **Step 2: Delete + docs.**
- [ ] **Step 3: Verify** — `cargo build --locked`, CI green, `grep -ri "python" README.md` shows no install instructions.
- [ ] **Step 4: Commit** (`chore(discord)!: remove Python implementation (Rust 0.2.0)`).

---

## Execution order

Tasks 1–4 in order (each builds on the last; Task 4 needs Task 2's text module). Task 5 needs Task 7's `prepare` signature — implement Task 7 first, or stub it in Task 5 and delete the stub in Task 7. Tasks 6, 8, 10, 11 need their listed producers. Task 9 is the big one — schedule it with a fresh worker. Tasks 12–15 only after 1–11 are green.
