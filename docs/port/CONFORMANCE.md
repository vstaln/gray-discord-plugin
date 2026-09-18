# gray-discord Conformance Audit (Python -> Rust 0.2.0)

This audit documents the 1:1 parity and intentional divergences between the legacy Python implementation (`gray_discord`), the Hermes Discord adapter text-gateway specification, and the standalone Rust binary (`gray-discord`).

Status legend:
- **PORTED**: Implemented in Rust with identical or strictly improved semantics.
- **DIVERGED**: Intentionally modified per design spec (e.g., stricter safety, compiled binary paths).
- **DEFERRED**: Explicitly slated for Phase 2 (voice/media/role auth).

---

## 1. Python Module Behavior Inventory

### `gray_discord.budget`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `validate(policy, model_name)` | `crate::budget::validate` in `src/budget.rs` | PORTED | Validates non-negative micro-USD bounds, model-string pinning, daily/turn limits. |
| `Budget.reserve(amount)` | `Budget::reserve` in `src/budget.rs` | PORTED | Checks cumulative daily usage + pending reservations; reserves micro-USD. |
| `Budget.settle(reservation, actual)` | `Budget::settle` in `src/budget.rs` | PORTED | Settles reserved vs actual token spending in SQLite ledger. |
| `Budget.accounted(since)` | `Budget::accounted` in `src/budget.rs` | PORTED | Sums micro-USD spent since timestamp window. |
| `Budget.total()` | `Budget::total` in `src/budget.rs` | PORTED | Aggregates all lifetime settled and pending micro-USD. |

### `gray_discord.capabilities`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `prepare(config, dest_dir)` | `crate::capabilities::prepare` in `src/capabilities.rs` | PORTED | Creates private scratch directory with isolated home; sets 0700 permissions. |
| Shared skills symlinking/copy | `src/capabilities.rs` | PORTED | Path traversal prevention (`..` rejection, canonicalization). |
| Shared context copying | `src/capabilities.rs` | PORTED | Non-secret files copied; sensitive credentials withheld. |
| Shared plugins validation | `src/capabilities.rs` | PORTED | Validates JSON plugin-argv format. |

### `gray_discord.cli`
| Python Command / Handler | Rust Subcommand | Status | Notes |
|---|---|---|---|
| `setup` | `Command::Setup` in `src/cli.rs` -> `crate::setup::run` | PORTED | Interactive setup wizard with TTY check. |
| `run` | `Command::Run` in `src/cli.rs` -> `crate::gateway::run` | PORTED | Runs gateway runtime with SQLite durable queue. |
| `sidecar` | `Command::Sidecar` in `src/cli.rs` -> `crate::sidecar::run` | PORTED | Stdio protocol 1.1 sidecar. |
| `register` | `Command::Register` in `src/cli.rs` -> `crate::service::register` | PORTED | Registers sidecar tool in `~/.gray/plugins/lock.json`. |
| `install` | `Command::Install` in `src/cli.rs` -> `crate::service::install` | PORTED | Installs systemd user service unit with user linger advice. |
| `uninstall` | `Command::Uninstall` in `src/cli.rs` -> `crate::service::uninstall` | PORTED | Disables and removes systemd unit file. |
| `status` | `Command::Status` in `src/cli.rs` -> `crate::service::control` | PORTED | Delegates to `systemctl --user status gray-discord-plugin`. |
| `stop` | `Command::Stop` in `src/cli.rs` -> `crate::service::control` | PORTED | Delegates to `systemctl --user stop gray-discord-plugin`. |
| `restart` | `Command::Restart` in `src/cli.rs` -> `crate::service::control` | PORTED | Delegates to `systemctl --user restart gray-discord-plugin`. |
| `doctor` | `Command::Doctor` in `src/cli.rs` -> `crate::doctor::doctor` | PORTED | Runs doctor checks with 30s timeout. |
| `share` | `Command::Share` in `src/cli.rs` | PORTED | Updates `shared_skills`, `shared_context`, `shared_plugins`. |
| `limits` | `Command::Limits` in `src/cli.rs` | PORTED | Configures timeout, concurrency, max requests. |
| `budget set` / `budget status` | `Command::Budget` in `src/cli.rs` | PORTED | Updates model-pinned budget policy; prints total micro-USD. |
| `queue list` / `queue cancel` | `Command::Queue` in `src/cli.rs` | PORTED | Queries inbox rows and cancels pending items in SQLite. |
| `schedule add` / `list` / `remove` | `Command::Schedule` in `src/cli.rs` | PORTED | Manages recurring interval jobs in SQLite with online CRUD. |
| `allowlist add` / `list` / `remove` | `Command::Allowlist` in `src/cli.rs` | PORTED | Snowflake-validated allowlist management (new in Rust port per §2). |
| Error envelope & credential redaction | `src/cli.rs::run()` | PORTED | Exits with code 1, prints sanitized user message, never leaks tokens or tracebacks. |

### `gray_discord.config`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `default_path()` | `crate::config::default_path` in `src/config.rs` | PORTED | `~/.config/gray-discord/config.json`. |
| `load_config(path)` | `crate::config::load_config` in `src/config.rs` | PORTED | Checks 0600 file permissions and parses JSON. |
| `save_config(path, data)` | `crate::config::save_config` in `src/config.rs` | PORTED | Atomic write via `.tmp` file and rename, permissions 0600. |
| `validate_config(data)` | `crate::config::validate_config` in `src/config.rs` | PORTED | Enforces required fields, snowflake IDs, positive limits. |
| `snowflake(v)` | `crate::config::snowflake` in `src/config.rs` | PORTED | 0 < ID < 2^64 numeric string validation. |

### `gray_discord.doctor`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `doctor(config)` | `crate::doctor::doctor` in `src/doctor.rs` | PORTED | Connectivity, authorization, channel permission, and binary check. |
| `/users/@me` validation | `src/doctor.rs` | PORTED | Confirms bot token is valid. |
| `/gateway/bot` check | `src/doctor.rs` | PORTED | Checks intents and rate limit recommendations. |
| Channel permissions check | `src/doctor.rs` | PORTED | Confirms bot has `READ_MESSAGE_HISTORY` and `SEND_MESSAGES`. |
| Gray installation check | `src/doctor.rs` | PORTED | Validates `gray_bin` executable and `gray_home` config. |

### `gray_discord.durable`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| SQLite schema (`inbox`, `outbox`, `schedules`, `meta`) | `src/durable.rs` | PORTED | Bundled SQLite with WAL mode and busy timeout. |
| `enqueue(...)` | `Store::enqueue` in `src/durable.rs` | PORTED | Message deduplication and transactional insertion. |
| `claim(...)` | `Store::claim` in `src/durable.rs` | PORTED | Single active worker per conversation/channel. |
| `complete(...)` | `Store::complete` in `src/durable.rs` | PORTED | Splits output text into parts and queues for delivery. |
| `fail(...)` | `Store::fail` in `src/durable.rs` | PORTED | Records terminal failure codes (`agent_failed`, `timeout`, `budget_blocked`, etc.). |
| `recover_interrupted()` | `Store::recover_interrupted` in `src/durable.rs` | PORTED | Crashed `running` items are failed as `interrupted` (no duplicate executions). |
| `next_delivery(...)` | `Store::next_delivery` in `src/durable.rs` | PORTED | Selects earliest pending outbox part honoring `next_at`. |
| `ack(...)` | `Store::ack` in `src/durable.rs` | PORTED | Records Discord message ID; sets `inbox.state = 'sent'` when all parts complete. |
| `delivery_failed(...)` | `Store::delivery_failed` in `src/durable.rs` | PORTED | Exponential backoff capped at 3600 seconds. |
| `prune_retention(...)` | `Store::prune_retention` in `src/durable.rs` | PORTED | 7-day retention cleanup of terminal inbox/outbox rows (Hermes recovery parity). |
| `migrate_jobs(...)` | `Store::migrate_jobs` in `src/durable.rs` | PORTED | One-shot migration from legacy `jobs.json` to SQLite schedules table. |

### `gray_discord.gateway`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `Runtime` & workers | `Runtime` in `src/gateway.rs` | PORTED | Generic async runner and deliverer with graceful shutdown. |
| Twilight event loop | `src/gateway.rs::run` | PORTED | Replaces discord.py with twilight-gateway Shard (`MessageCreate`, `InteractionCreate`). |
| Multi-worker generation | `src/gateway.rs` | PORTED | `N = concurrency` worker tasks bounded by semaphore. |
| Delivery background loop | `src/gateway.rs` | PORTED | Dedicated delivery task consuming outbox parts with stable nonces. |
| Schedule ticker | `src/gateway.rs` | PORTED | 1-second ticker checking due schedules and enqueuing items. |
| Slash command registration | `src/gateway.rs` | PORTED | Bulk overwrite with app ID; hash caching via `meta` table. |
| Slash command handlers | `src/gateway.rs` | PORTED | `/ask` (allowlisted prompt), `/reset` (session clear), `/status` (queue depth), `/stop` (cancel). |

### `gray_discord.policy`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `incoming(message, config, pairing_code)` | `crate::policy::incoming` in `src/policy.rs` | PORTED | Admission policy gate. |
| Bot message dropping | `src/policy.rs` | PORTED | Ignores bot messages. |
| Mention check & stripping | `src/policy.rs` | PORTED | DM or mention required; `<@ID>` and `<@!ID>` stripped from content. |
| Owner & allowlist check | `src/policy.rs` | PORTED | Admits `owner_id` and IDs present in `allowed_users`. |
| Pairing code matching | `src/policy.rs` | PORTED | Constant-time comparison during interactive setup. |

### `gray_discord.runner`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `run_turn(...)` | `crate::runner::run_turn` in `src/runner.rs` | PORTED | Child process execution of `gray -p <prompt> --json`. |
| Environment sanitization | `src/runner.rs` | PORTED | Filters out sensitive prefixes (`GRAY_`, `DISCORD_`, `OPENAI_`, `ANTHROPIC_`). |
| Child process isolation | `src/runner.rs` | PORTED | Private working directory and isolated gray home. |
| NDJSON event streaming | `src/runner.rs` | PORTED | Parses streaming JSON lines (`agent_state`, `content`, `done`, `error`). |
| Session tracking | `src/runner.rs` | PORTED | Persists session ID in `session.json` per conversation. |

### `gray_discord.service`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `unit(config_path)` | `crate::service::unit` in `src/service.rs` | DIVERGED | Generates systemd user unit using compiled binary `gray-discord run` instead of Python `-m`. |
| `quote(s)` | `crate::service::quote` in `src/service.rs` | PORTED | Systemd unit path escaping (% -> %%, whitespace escaping). |
| `control(args)` | `crate::service::control` in `src/service.rs` | PORTED | Executes `systemctl --user <args>`. |
| `install(path)` | `crate::service::install` in `src/service.rs` | PORTED | Writes unit file, runs daemon-reload, enables & starts service. |
| `uninstall()` | `crate::service::uninstall` in `src/service.rs` | PORTED | Disables service, deletes unit file, runs daemon-reload. |
| `register(config, config_path)` | `crate::service::register` in `src/service.rs` | PORTED | Updates `~/.gray/plugins/lock.json` with binary argv. |

### `gray_discord.setup`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `run(...)` | `crate::setup::run` in `src/setup.rs` | PORTED | Full interactive pairing wizard. |
| Hidden terminal token input | `src/setup.rs` | PORTED | Disables terminal echo during token entry. |
| Invite URL generation | `src/setup.rs` | PORTED | `https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot&permissions=68608`. |
| One-time DM pairing code | `src/setup.rs` | PORTED | Generates 18-byte URL-safe code, 300s expiration, validates DM. |
| Local owner confirmation | `src/setup.rs` | PORTED | Confirms user ID in terminal before persisting. |

### `gray_discord.sidecar`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| Protocol 1.1 NDJSON | `src/sidecar.rs` | PORTED | Handles newline-delimited JSON on stdin/stdout. |
| `plugin/manifest` | `src/sidecar.rs` | PORTED | Returns static manifest (`name: discord`, `version: 0.1.0`, `protocol: 1.1`). |
| `prompt/context` | `src/sidecar.rs` | PORTED | Emits prompt hook guidance. |
| `tool/call` (`discord_send`) | `src/sidecar.rs` | PORTED | Sends message to home channel via REST transport. |
| Secret suppression | `src/sidecar.rs` | PORTED | Returns generic error envelope without leaking token or configuration. |

### `gray_discord.text`
| Python Symbol / Behavior | Rust Implementation | Status | Notes |
|---|---|---|---|
| `split_message(text, limit)` | `crate::text::split_message` in `src/text.rs` | PORTED | UTF-16 code unit boundary splitting, paragraph/line preference. |
| Mention suppression | `src/transport.rs` | PORTED | Outgoing REST payload specifies `allowed_mentions: { parse: [] }`. |

---

## 2. Hermes Phase-1 Parity Inventory

| Feature | Reference | Rust Implementation | Status | Notes |
|---|---|---|---|---|
| UTF-16 safe chunk splitting | Hermes `_split_message` | `src/text.rs` | PORTED | Splits at UTF-16 code units (<=2000 chars) on natural line breaks. |
| 8-chunk output cap | Hermes `_cap_split_chunks` (`MAX_SPLIT_MESSAGES=8`) | `src/durable.rs`, `src/gateway.rs` | PORTED | Chunks beyond 8 replaced with truncation notice indicating dropped characters. |
| Processing reaction (`👀`) | Hermes `on_processing_start` | `src/gateway.rs` | PORTED | Added on claim of turn; failure to react does not fail the turn. |
| Completion reactions (`✅` / `❌`) | Hermes `on_processing_complete` | `src/gateway.rs` | PORTED | Removes `👀` and posts `✅` on delivery completion or `❌` on error/timeout/cancel. |
| Typing indicator cadence | Hermes typing loop | `src/gateway.rs` | PORTED | Sends typing POST every 8 seconds during turn generation. |
| Slash command registration | Hermes `_safe_sync_slash_commands` | `src/gateway.rs` | PORTED | Bulk overwrite with SHA-256 hash caching in SQLite `meta` table. |
| `/ask` slash command | Hermes `/ask` | `src/gateway.rs` | PORTED | Submits prompt; defers interaction, routes followup reply. |
| `/reset` slash command | Hermes `/reset` | `src/gateway.rs` | PORTED | Resets caller's session pointer in conversation directory. |
| `/status` slash command | Hermes `/status` | `src/gateway.rs` | PORTED | Replies ephemerally with pending queue depth. |
| `/stop` slash command | Hermes `/stop` | `src/gateway.rs` | PORTED | Sets cancel flag on the caller's active turn. |
| Allowlist authorization | Hermes `_is_allowed_user` | `src/policy.rs`, `src/cli.rs` | PORTED | Snowflake allowlist; defaults to owner-only when empty. |
| Thread isolation | Hermes conversation thread keys | `src/gateway.rs` | PORTED | Uses channel ID as conversation key; replies routed to thread. |
| 7-day queue retention prune | Hermes `recovery.py` | `src/durable.rs` | PORTED | Prunes terminal inbox/outbox items older than 7 days. |

---

## 3. Explicitly Deferred Features (Phase 2)

The following capabilities from Hermes full suite are deliberately deferred to Phase 2 per design spec (§3):
1. **Voice / Opus / TTS**: Voice channels, real-time voice synthesis, audio mixing.
2. **Media & Attachments**: Image downloads, OCR, file uploads, Discord rich embeds.
3. **Channel Skill Bindings**: Restricting specific gray skills to specific Discord channels.
4. **Free-Response Channels**: Responding to every message in a channel without an `@mention`.
5. **Role-Based Authorization**: Authorizing users based on Discord guild roles rather than snowflake user IDs.
6. **Missed-Message Backfill**: Retroactively processing messages missed during gateway downtime (the SQLite durable queue is the sole reliability mechanism).
