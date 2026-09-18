# gray-discord Rust port — design spec

- Date: 2026-09-18. Status: user-approved (§1–§4 agreed in chat).
- Goal: replace the Python `gray_discord` package with a single Rust binary
  in this repo, reaching Hermes text-gateway parity (Phase 1). Voice/media is Phase 2.
- Non-goals: gray-core changes (except the discord CATALOG pin, done separately
  in the gray repo), Telegram/Slack, an in-process agent, role auth, allow-all.

## References (read during design)

- Python source of record: `gray_discord/*.py` (~1,160 lines) + `tests/*.py`
  (660 lines) at `8b687df`, this repo.
- Hermes adapter: `plugins/platforms/discord/adapter.py` (10,630 lines),
  `recovery.py` (112), `voice_mixer.py` (387), `ffmpeg_utils.py` (43),
  `plugin.yaml` env contract, `tools/discord_tool.py` (1,116).
- Salvage: gray history `db3411c` `crates/gray-gateway/src/discord.rs`
  (942 lines, twilight 0.17 per its Cargo.toml — header comment says 0.16 —
  slash `/ask /reset /status /stop`) plus
  `platform.rs`/`delivery.rs`/`pairing.rs`/`authz.rs`/`systemd.rs` shapes;
  hermes-rs `hermes-tools/discord_tool.rs` (REST helpers) and
  `hermes-gateway/authz_mixin.rs` (`coerce_allow_set`).
- Install pattern: `gray-background` (prebuilt release binaries + SHA-256
  sidecar + manifest verification, `gray install plugin <name>`).

## §1 Architecture

One Rust binary, `gray-discord`, two modes in one process, following the
`gray-background` standalone-binary pattern:

- `gray-discord sidecar --config <path>`: protocol-1.1 stdio sidecar.
  Methods: `plugin/manifest` (`discord` 0.1.0, `discord_send` tool,
  `prompt/context` hook), `tool/call`, `prompt/context`. Wire rules ported
  from `sidecar.py`: 256 KiB frames, numeric ids only, notifications ignored,
  unknown methods get `{"error": ...}`, `plugin/shutdown` exits, delivery
  failures return `is_error` without leaking config/token text.
- `gray-discord run --config <path>`: the gateway. One Discord connection per
  token (twilight gateway+REST, version chosen at implementation — latest
  0.1x verified against docs.rs then; 0.17 is stale). Agent stays
  out-of-process: one `gray -p <prompt> --json --max-requests N
  [--session SID]` child per turn, NDJSON protocol-1 rows, explicit
  `session.json` pointers per conversation, isolated homes — the exact safety
  model of `runner.py`. Nothing in-process (the deleted `gray-gateway` crate's
  tight coupling is not repeated).
- CLI surface keeps every Python command with identical semantics:
  `setup run sidecar register install status stop restart doctor uninstall
  limits budget share queue schedule`, plus a new `allowlist add|remove|list`
  for §2. `--config` precedes the subcommand on every invocation, as today.
- Module layout mirrors the Python package so review stays 1:1:
  `config policy text sidecar transport gateway runner durable budget
  capabilities service setup doctor`, plus `cli`/`main`. Each module gets a
  behavior-inventory table against its Python source in the implementation plan.

## §2 Auth and config

- `config.json` (mode 600, atomic replace, same path
  `~/.config/gray-discord/config.json`) keeps every current field and adds
  `allowed_users`: array of Discord snowflakes, default `[]`. Empty means
  owner-only. Validation ports `config.py` + `budget.validate` exactly
  (snowflake shape, absolute paths, int ranges, budget/model pin).
- Per-message gate (ports `policy.incoming` + Hermes `_is_allowed_user`
  minus roles): drop bots → require DM or @mention (`<@ID>`/`<@!ID>` stripped)
  → admit iff sender is `owner_id`, in `allowed_users`, or pairing-approved
  mid-setup. Anything else is a silent drop plus a warn-once log line
  (Hermes' fail-closed warning). No roles, no allow-all flag, no `"*"`
  wildcard in v1 — recorded divergences, not oversights.
- Pairing setup flow ported unchanged from `setup.py`: TTY check, gray
  binary/home prompts, provider model check, budget policy prompts, hidden
  token input, login, invite URL (`scope=bot&permissions=68608`), Message
  Content Intent reminder, one-shot DM code (`token_urlsafe(18)`, 300 s,
  constant-time compare), local owner-ID confirmation, home-channel choice,
  private save, sidecar registration, optional service install.
- Allowlisted users may chat (turns billed to the owner's budget ledger).
  Everything else stays owner-only and CLI-local: `schedule`, `budget`,
  `share`, `register`, `allowlist` have no chat/slash surface. Slash v1:
  `/ask` (allowlisted, required `prompt` option), `/reset` `/status` `/stop`
  (owner-only; reset clears the caller's session pointer, stop cancels the
  running turn without replacement). Commands registered on connect.
- Secrets: token never logged, never sent to the model, stripped from the
  `gray -p` child env (`GRAY_`/`DISCORD_`/`OPENAI_` prefix filter, as today);
  error paths print categories, never SDK/config bodies.

## §3 Delivery and runtime

- Send path (ports `transport.py` + Hermes `send`/`_cap_split_chunks`):
  REST post to the configured home channel, UTF-16-safe 2000-char chunks on
  line boundaries (the `hermes_text.py` algorithm, surrogate-safe), all
  mentions suppressed (`parse: []`, no reply pings — stricter than Hermes, as
  today), a per-turn cap of 8 chunks with truncation notice (Hermes
  `MAX_SPLIT_MESSAGES = 8`, `adapter.py:1058`). `discord_send` targets the home
  channel only. Bounds: 1–20000 chars inbound to send, terminal answers
  1–200000 chars.
- Inbound: single gateway connection, per-conversation locks (one turn at a
  time per channel; second message gets the busy reply), typing indicator
  refreshed on the 8 s cadence during turns. Threads work via channel-id
  conversation keys (thread has its own id; replies stay in-thread).
  Reactions: port `on_processing_start`/`on_processing_complete`
  (`adapter.py:3356-3383`): add 👀 on accept, swap to ✅ on success
  / ❌ on failure, gated on a reactions-enabled flag.
- Durability (ports `durable.py` + `budget.py` to `rusqlite` bundled, same
  schema semantics): SQLite inbox/outbox/schedules/meta. Dedupe message IDs,
  per-conversation serialization, generation separate from delivery retries
  with exponential backoff (cap 1 h), restart marks running work `uncertain`
  (never auto-rerun side effects), per-chunk message-ID receipts, at-least-once
  delivery stated honestly. Budget ledger: per-turn reserve before spawn,
  settle on complete accounting, unknown/crashed/cancelled usage retains the
  reservation, model-pinned policy required before `run` starts. Add one Hermes
  item the Python lacks: 7-day retention prune of terminal inbox rows
  (`recovery.py` parity; archives/ledgers are operator data, never pruned to
  evade limits).
- Deferred to Phase 1.5/2 (explicit, not dropped): role auth, free-response
  channels (guilds stay mention-gated), channel skill bindings, missed-message
  backfill (the durable queue is the reliability mechanism; backfill would
  double-deliver), full slash catalog/model picker, voice/Opus/TTS,
  attachments/embeds beyond text.

## §4 Install, migration, testing

- Release: copy the `gray-background` workflow (tagged `v*`, four targets:
  linux x86_64/aarch64-musl, macOS x86_64/aarch64, `cargo test` +
  `cargo fmt --check` gating, binary + `.sha256` assets). gray-core
  `plugin_cli.rs` CATALOG discord entry moves from the Python git pin to the
  release asset (separate commit in the gray repo, listed as integration step).
- Migration: reuse existing paths/schemas as-is (`config.json` +
  `allowed_users: []` default, `queue.sqlite`, `budget.sqlite`,
  `conversations/*`, legacy `jobs.json` one-shot import). Python package is
  deleted from this repo only after the Rust suite is green and a release
  asset installs cleanly.
- Testing: port every `tests/*.py` file to Rust integration tests —
  budget ledger, capabilities, core (config/text/admission/pairing),
  delivery (loopback HTTP stub for Discord REST, no live tokens), durable
  queue, package (help/manifest/registration/service unit), runner (fake
  `gray` fixtures + `GRAY_TEST_BIN` real-gray tests), runtime (restart/
  cancel semantics), schedule CLI, setup CLI. No network, no credentials.
  `cargo test` runs in CI/TTY only (X-session ban); local checks are
  `cargo fmt --check`, `cargo build`, `cargo clippy -- -D warnings`.

## Acceptance

Phase 1 is done when: all Python behaviors are ported with tests green, the
Hermes Phase-1 behavior inventory (admission, chunking, threads, reactions,
4 slash commands, pairing, recovery prune, channel scoping) is covered or
listed as deferred above, a tagged release installs via
`gray install plugin discord`, existing configs migrate without loss, and the
Python package is removed. Phase 2 (voice/media) gets its own spec.
