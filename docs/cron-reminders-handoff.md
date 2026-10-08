# Handoff: Discord cron reminders are broken in the field — fix plan

Written for the next implementer (Claude). Two repos are involved:

- **gray core**: `/home/vstaln/gray` — Rust workspace, `main` at `824e60fe` (v0.1.12)
- **discord plugin**: `/home/vstaln/grayplugins/gray-discord-plugin` — Rust, `main` at `4076a45` (v0.1.1)

Read `/home/vstaln/gray/AGENTS.md` before working in gray: `CARGO_BUILD_JOBS=4`, narrow `cargo test -p` during iteration, full `cargo test --workspace` + `cargo fmt --check` once before commit, never push to `main` without asking, fold work onto an existing open PR branch if one exists.

## The incident

In a Discord channel, the user asked for a reminder in one minute. The gray agent (spawned per-turn by the plugin):

1. Ran `gray cron add "in 1m" "Remind the user: please go to the bathroom" --reminder` — succeeded (0.0s).
2. Its stdout included: `⚠ no cron ticker has ever run — this job will not fire until one does; install the gateway (gray gateway install), or run gray cron serve, gray cron tick, or a REPL`.
3. The model obeyed the warning literally: spawned `gray cron serve` inside the turn (killed at the 30s command timeout), then `sleep 70` (killed at 30s), then `true` no-ops. The Discord turn **failed after 1m43s, 8 commands**.
4. **The reminder never appeared in the channel.**

## Verified ground truth (from disk, not speculation)

- The job landed correctly: `~/.config/gray-discord/conversations/54705651de8959d50e95667f2a4ce2b4160eacbc7d6f2b06041a2fe06892b6cf/cron/jobs.json` holds `ff1f37413516` with `deliver: "origin"`, `origin.route: "1544925612823547987"`, `reminder: true`, `state: "done"`, `last_status: "ok"`, `last_run_at: 1791384438` (due 1791384436, fired +2s). Output at `cron/output/ff1f37413516/1791384438.md` contains the reminder text.
- **The plugin never posted it**: `post_delivery` unconditionally `eprintln!`s `[discord] cron full output: {path}` before posting (`src/cron.rs:212-214`), and no such line exists for this job in `/home/vstaln/.gray/logs/discord-run.log` (only two lines for a different conversation home).
- **Who fired it**: the agent's `cron serve` ran 30s and was killed ~30s before the due time — its next interval tick would have been too late. The turn ran **8 commands**; the surviving evidence (fired exactly at 21:47:18, `kind:"cli"` ticks on the home, no plugin post) fits an in-turn probe (`gray cron tick` / `cron run` / `cron list`-adjacent) claiming and firing it. Whichever in-turn driver it was, it consumed the delivery on its own stdout — `rep.delivered` is a return value that dies with the caller. The plugin's own 60s tick (verified live: `.last_tick` `kind:"cli"` stamps continue afterward) arrived to find the job already `done`.
- Note: mirror-append failure does NOT suppress delivery (`cron_serve.rs:218-228` logs and returns `to_chat: true`), so the silent-loss path is specifically "a non-plugin driver fired it", not a missing session file.

## The architecture that already exists (do not rebuild)

**Core** (`crates/gray/src/`):
- `cron/store.rs` — `CronStore` at `$GRAY_HOME/cron/`, jobs.json + `.last_tick` heartbeat + claim (`fire_claim`) + `CronHealth`.
- `cron_serve.rs` — `tick_once` (claim→fire→record), `serve_loop` (60s), `HeadlessRunner` (runs a real agent turn), `SaveLocalDeliver` (writes `cron/output/<job>/<ts>.md`; `Deliver::Origin` with platform `"repl"` writes `cron/inbox/<session>/`; other platforms append a session mirror and return `to_chat: true`).
- `cron_status.rs` — `ticker_line` and `add_warning` (the misleading warning lives here; `TICKER_STALE_SECS` liveness horizon).
- `main.rs` `CronCmd::Add` (~line 570-672): reads `origin_from_env()` (`GRAY_CRON_ORIGIN` JSON `{platform,chat,route}`) and `session_origin_from_env()` (`GRAY_SESSION_ID`); env origin makes the job `Deliver::Origin` by default. Prints `add_warning` after "added …".
- `CronCmd::Tick { json }` (~line 725-756): `tick --json` emits one `cron_delivery` JSON line per fired origin job (via `cron_serve::delivery_json`) — **this is the only wire chat-bound deliveries ever leave core on**.
- `gateway/run.rs` — the supervised gateway ticks `~/.gray/cron` every 60s but **drops `rep.delivered`** (only logs counts). Same for `serve_loop`.

**Plugin** (`gray-discord-plugin`):
- Per-conversation gray homes: `<config>/conversations/<sha256(conversation)>/`, each with its own `config.json`, `route.json` (`{platform:"discord", chat, route:<channel_id>}`), and own cron store.
- `src/runner.rs:418-422` — every spawned turn gets `GRAY_CRON_ORIGIN` from `route.json`, so a bare `gray cron add` inside a turn binds to the channel.
- `src/gateway.rs:1424-1440` — daemon cron task: every 60s, `routable_homes()` (homes with `cron/` dir + `route.json`) → `tick_home()` (`gray cron tick --json`) → `parse_tick()` → `post_delivery()` posts a Components V2 card to `origin.route` (plain-text fallback on 400, one rate-limit retry).
- `src/cron_card.rs` renders the card (kind/status/elapsed; reminder jobs show no timing).

**Structural race**: `routable_homes` requires the `cron/` dir to exist, which only happens once a job is added — so the FIRST `cron add` in a fresh conversation always sees `.last_tick` absent → the "no ticker has ever run" warning fires. That warning is what sent the model down the `cron serve` path.

## What Hermes does (NousResearch/hermes-agent + hermes-rs local checkout)

- `~/.hermes/cron/jobs.json`; multiple tick producers (gateway, standalone daemon, systemd timer) share it. Delivery context is captured at create time via env (`HERMES_SESSION_PLATFORM`, `HERMES_CRON_AUTO_DELIVER_CHAT_ID`) — the fired job auto-delivers to the chat it came from.
- **The parity contract**: `cronjob(action="create")` returns the moment the record is saved — the agent never babysits; the ticker owns firing AND delivery independently of any session. Delivery survives the creating session's death (standalone send path).
- `mark_job_run` splits `last_status` from `last_delivery_error` — a job can fire fine and fail delivery as separate columns; completed jobs are retained (`state:"completed"`), not popped.
- Reliability extras worth noting: `failure_streak` + nudge threshold, `plan_retry` re-runs fires that never reached the model (5/15/30m), `recover_interrupted_executions` at ticker start (claimed/running rows → unknown only when the owning pid is provably dead), gateway drain waits ≤30s for in-flight fires then marks interrupted, `executions.db` ledger for catch-up/audit, delivery confirmation ledger.
- PR #10443 (open): hardening — JSONL `cron-events` log; stale-oneshot detector (>120s past due without run → alert); completed-job retention 7d; per-job `retry_policy`/`idle_timeout_seconds`; `fcntl` write lock on jobs.json. Known wart: `tick()` holds a lock across whole job execution (head-of-line blocking).
- PR #123139: `schedule_wake` tool — agent arms a one-shot deadline on its OWN session; the owning driver's idle loop fires it into the turn queue (~2s). Hidden by `check_fn` where no driver exists (prevents arming dead state). Claim-first at-most-once.
- Also #87877 (relative-reminder timezone safety), #129006 (level-triggered monitor reminders), #22044 (ambient cron mode).

## What OpenClaw does (openclaw/openclaw, TypeScript)

- Cron jobs carry `sessionTarget` — `main` resolves the owning agent's main session and omits a session key so the Gateway queues it there; the reminder runs **as a turn in the owning conversation** and delivers via the normal reply path (#125198).
- Heartbeat cadence is system-owned cron work; script automations return `wake:"now"|"next-heartbeat"` — immediate targeted notification event vs deferred (#129936, #112585).
- Channel-created jobs carry their channel routing (#156966).
- Reliability posture: recover missed reminders across DST (#129478), report overflow instead of silently dropping (#165305), record designed skips as no-ops (#145242), qa proves natural firing (#123127).

Gray's chat-bound design already resembles OpenClaw's (per-conversation homes + origin routing + host tick). What's missing is durability + the honest warning.

## The fix (proposed, minimal-first)

### 1. Suppress the false driver warning for chat-bound adds — core, small

`cron add` prints `add_warning` (`main.rs:~667`) whenever the home's `.last_tick` is absent/stale. When the job is origin-bound (env origin present), that warning is wrong and harmful — the conversation's host ticks the store itself. Reword/replace for that case, e.g. `delivers back to this conversation on the host's next tick (~60s); nothing further to do`. Still warn when the host tick is genuinely absent if detectable — but at minimum never tell the model to `cron serve` a chat-bound store. Signature change: `cron_status::add_warning(last_tick, now, origin_bound)`.

### 2. Don't let the wrong driver eat chat-bound deliveries — core, the real bug

Today a `cron_delivery` line is emitted only by `tick --json` at claim+fire time; any other driver firing an origin job loses the delivery forever (serve drops `rep.delivered`; gateway drops it; bash-captured stdout dies). Options, in increasing robustness:

- a) `cron serve`/`cron tick` (non-json) firing an `origin` job records the delivery into the store as **pending** (e.g. `job.pending_delivery` or `cron/outbox/<job>-<ts>.json`); `tick --json` emits pending deliveries as `cron_delivery` lines and marks them emitted; a new `gray cron delivered <id>` (or `--ack` flag on tick) clears them after the host confirms the post. Plugin calls the ack only when `post_delivery` returned true → at-least-once posting.
- b) Cheaper: any driver that isn't the chat host leaves origin deliveries **unclaimed for delivery** — fire+record only, set `last_status: delivery_pending`, and let `tick --json` (which the plugin runs every 60s) drain pending deliveries. Same pending store, no ack verb needed; a failed host post retries next tick until the job row's retention expires.

Either way: firing a job must never again be the moment a channel-bound delivery is decided-and-lost. (Hermes' equivalent: `mark_job_run` keeps `last_delivery_error` as a column separate from `last_status`, plus a delivery ledger — the delivery leg is recorded independently of the fire outcome. Gray has `last_delivery_error` already; what's missing is retaining the *payload* to retry.)

### What gray already has (do not re-implement — verify instead)

From the deep read of `crates/gray/src/cron/`: claim TTL 1200s reclaim, advance-before-fire, stale-recurring fast-forward, one-shot 120s grace→done, `.last_tick` heartbeat with driver `kind` tags (`serve|cli|repl|gateway`), `MAX_CONCURRENT_FIRES=4`, `[SILENT]` suppression, `Deliver::{Local,Origin,Target}`, `cron/inbox/<sid>` session wake path, `/cron off` master switch, `refresh_model_from_saved` so long-lived tickers follow `/model` switches. The fire leg is solid — the gap is delivery durability + the warning.

### 3. Agent UX close-out — mostly free after (1)

With the correct add output the model has no reason to babysit. Optionally have `cron add` for `--reminder` print `fires at <ts>; delivers here` and nothing else.

### 4. Other confirmed gaps worth fixing (ordered by how silently they lose reminders)

- **One-shots die silently at >120s lateness.** `claim_due_limited` retires a one-shot more than `ONESHOT_GRACE_SECS` (120s) past due to `state: Done` + `enabled: false` + `last_error: "missed one-shot window"` (`store.rs:596-603`) — a "remind me in 1m" dies if the plugin daemon is down for ~3 minutes past due. Hermes' equivalent alerts on stale one-shots and keeps them auditable (#10443); gray should at minimum make this LOUDER (delivery of the failure, not just a field) or give reminders a longer grace.
- **Non-json `cron tick` also eats deliveries.** `rep.delivered` is only serialized by `tick --json`; plain `tick`, `cron run`, `serve`, `gateway`, and REPL-adjacent fires all consume the claim + mark done. This is the same root bug as #2 — fix once in the store/delivery seam, not per-driver.
- **`cron serve` has no SIGTERM handler** (only `ctrl_c`) — `sv stop` / systemd kills it mid-claim; harmless today (claim TTL reclaims) but sloppy.
- **Agent-facing docs steer the model into this trap.** `gray-skill.md:81-83` tells the model "the supervised gateway daemon fires the jobs" — wrong for chat-bound homes — and the bash tool description says a `--reminder` "wakes this session" (true only for `platform:"repl"`). Reword so the model understands: jobs added here post back to this channel on the host's cadence; do not run `cron serve`/`tick` to check on it.
- `cron/inbox/<session>` entries never expire; `Deliver::Target` is a dead letter (warns "unknown target"); `run.rs` comments say claim TTL 300s but it's 1200s; plugin `sidecar.rs` manifest version is stale (`0.1.0` vs `0.1.1`); plugin README cron section + `docs/RUNTIME.md:161-164` claim plugin `cron add` "would never fire" (predates the tick bridge — now false).
- Longer-tail parity: cron JSONL event log, 7d completed retention with GC, `jobs.json` flock (exists: `.jobs.lock` 30s deadline), per-job retry/backoff, `failure_streak` nudges, executions ledger, `--deliver discord:<channel>` via a real `Deliver::Target` backend.

## Verification recipe

1. Fresh temp home + `route.json` + `GRAY_CRON_ORIGIN` set: `gray cron add "in 1m" "x" --reminder` → assert no driver warning, assert the new close-out message.
2. **The incident reproduction**: same setup, fire the job via a NON-plugin path (`gray cron tick` without `--json`, or `cron serve` for one pass, or `cron run <id>`) → assert the delivery survives into a pending state and a subsequent `gray cron tick --json` still emits the `cron_delivery` line. Today this test would fail — the delivery is consumed at fire time and `state:"done"` records `ok`.
3. One-shot lateness: a `Once` job >120s past due → assert it does not silently retire (whatever louder semantics are chosen — retry, longer reminder grace, or a delivered failure notice).
4. Plugin side: `tick_home` on a home with a pending-but-fired job → `parse_tick` yields the Delivery; `post_delivery` failure leaves the delivery pending so the next tick retries.
5. Full `cargo test --workspace` (gray) and `cargo test` (plugin) + `cargo fmt --check`. Existing suites to extend: `cron_serve_tests.rs`, `cron_status_tests.rs`, `cron_fire_tests.rs`, `cron/store_tests.rs`; plugin `tests/` has cron card/parse tests.

## Production state at handoff

- `~/.cargo/bin/gray` = 0.1.12; `~/.gray/plugins/discord/gray-discord` = 0.1.1-equivalent local build; runit services `gray-gateway` (pid 31065) and `gray-discord` (pid 21180) running.
- After core changes land: rebuild + `cargo install --bin gray`, rebuild plugin binary, `sv down/up gray-discord` to swap (copy fails with `text file busy` while running).
