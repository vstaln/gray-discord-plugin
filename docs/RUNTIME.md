# Runtime reliability changes (isolated development version)

Requires the matching gray core changes implementing `gray -p --json` and
`gray --input-json` protocol 1. Do not install this branch with an older gray
executable. Upgrade the gray binary and this plugin together, then restart the
service so the two protocol versions match.

## Structured execution

The runner consumes NDJSON, not terminal output or session-file text searches.
Progress contains safe phase/tool labels, redacted one-line details, and a
bounded redacted output for completed tools (plus an internal call ID for
pairing parallel results). With `GRAY_STREAM_TEXT=1` the stream also carries
the assistant's prose as `text` rows: per segment (a run of prose between tool
calls), append-only `delta` lines plus a provisional `tail`, closed by a
`done` row. Discord drains rows per conversation into a stream of Components
V2 messages stream like a chat feed: each run of prose is a new message, each group of
tool calls a new tool bubble below it, with a status chip (Stop button, an
accent that tracks the outcome) on the newest. The turn's messages are driven
beside the gray child, never between reads of its output, so a slow Discord
request cannot stall the agent. The answer's last edit becomes the durable
answer (see README, "Live replies"). Raw reasoning is suppressed before it reaches the bridge. The
terminal result includes
`session_id`,
`turn_id`, final redacted assistant text, and provider accounting. Errors are
sanitized and retain a nonzero exit status. Malformed output is rejected.
Each conversation keeps an explicit session pointer. Existing single-file
conversation homes migrate on first use; ambiguous stores fail closed. DMs use
the channel key, native Discord threads use their thread channel key, and guild
messages use a per-user key. The default reset policy is `both`: 1,440 idle
minutes or the next 04:00 local boundary. `/new` and `/reset` remove the
conversation's JSONL transcripts and advance a generation marker.

### Typed component turns

The plugin runner supports two input modes. Ordinary messages retain `-p` and
legacy text semantics. Normalized Discord components are written to a private
`turn-input.json` beneath the conversation home and invoke the native command:

```sh
gray --input-json /path/to/turn-input.json --json --session <id>
```

The file is mode 0600, removed after the child exits, and never placed in
argv. Gray parses the `gray.discord.input` version-1 envelope, preserves it as
a typed user block, and continues the same session. Component state tokens and
Discord interaction tokens stay in the plugin's SQLite rows; managed file IDs
are metadata, not file contents.

`gray discord limits --timeout-seconds 1800 --concurrency 4 --max-requests 200`
sets runtime policy; restart to apply. Defaults: 600 seconds, 2 workers, 200
provider requests per turn. Workers serve both chat and scheduled jobs.
`gray discord queue list` shows accepted IDs and states; `queue cancel ID`
cancels queued work or signals its running process group. A cancellation may
occur after a tool side effect. Do not assume cancellation rolls back work.

## Recovery

Private SQLite inbox/outbox commit accepted messages before execution, deduplicate
Discord message IDs and serialize each conversation. A generation result and its
reply chunks are recorded together. Failed delivery retries with backoff without
regenerating the answer. Each successful chunk records its Discord message ID.
A restarted worker marks interrupted agent execution `uncertain`, rather than
repeating possibly executed actions. Budget-blocked and failed runs are visible
in the queue and generate a controlled diagnostic reply.

Delivery is **at least once**, not exactly once. A network failure after Discord
accepts a send but before the response is recorded can duplicate that chunk.
The SDK supports a nonce but does not expose enforced nonce deduplication in
this version. Messages sent while the bot is disconnected and never accepted
into the inbox are not recovered by this queue. The standalone `discord_send`
tool is a direct REST action, not an outbox transaction; its errors must not be
blindly retried. Shared plugins can also have their own side effects.

### Restart and shutdown notices

Owned by gray core:
`gray gateway lifecycle boot|stop --dir <config dir>` decides how the last run
ended and words every notice, so Telegram or Slack adapters get the same
behaviour; this plugin only posts the text. Against a gray that predates the
subcommand it falls back to a local copy that uses the same files. On SIGTERM or Ctrl-C, while
the connection is still up, every chat with a running turn is told the task is
about to be interrupted, and the home channel gets a short "restarting" or
"shutting down" line (one message per chat, bounded to 4 seconds).
`gray discord restart` leaves a `restart_pending` marker so the daemon says
"restarting" rather than "shutting down".

`lifecycle.json` next to the config records `running` or `stopped`. On the next
boot the home channel hears "Gateway restarted", "Gateway online", or, when the
file still says `running` (crash, SIGKILL, OOM, reboot), that the gateway is
back after an unexpected stop, with how many turns were cut short. A first boot
says nothing. Set `"restart_notification": false` to silence all of it.

## Spending policy

A budget policy is optional accounting, never a start requirement: the setup
wizard offers it (default no), and gray's own `gray gateway setup discord`
flow writes none. An existing policy must set daily/turn allowances and
explicit prices for the selected model:

```sh
gray discord budget set --daily-usd 5 --turn-usd 0.50 \
  --input-per-million 1 --output-per-million 3
gray discord budget status
```

**These prices are examples, not a model recommendation.** Enter prices covering
all billed input/output and reasoning. Explicit zero rates are allowed only for
genuinely free models. A model change invalidates the stored price policy.

Each turn reserves its allowance in a persistent ledger before launching gray.
Concurrent turns share the ledger. Complete accounting settles the reservation;
unknown usage, crashes or cancellation retain it, including across midnight.
Daily settled charges use UTC dates. Status displays lifetime charged/reserved
micro-USD; it is not an invoice. There is no automatic forgiveness of uncertain
reservations. Retain the ledger for audit; never delete it to evade limits.

Gray meters provider invocations including compaction and tool rounds, and stops
subsequent calls when the request/cost cap is reached or budgeted usage is unknown.
**Not a hard provider invoice cap:** an already-sent request can exceed the
remaining allowance, internal provider retries can incur charges not fully
reported, and rates/token reports can be inaccurate. Set provider-side billing
limits for a hard ceiling. External tools and shared plugins may incur charges
outside this model ledger.

## Sessions and selected capabilities

The removed 32 MiB session-scraping limit no longer blocks Discord replies.
Before JSON-mode resumption, gray streams maintenance of large (>8 MiB) valid
linear sessions containing compaction markers. Superseded entries move out of the
hot file; a full original copy is retained under `sessions/archive`. The active
summary/messages and session ID remain. Branches and damaged histories are left
to the normal loader. A record over 16 MiB is not rewritten by maintenance.

This does not bound all gray loader memory: an enormous history with no
compaction marker still follows gray's existing loader/compaction path. Archives
are retained indefinitely; disk retention remains an operator responsibility.

Select capabilities rather than copying all desktop configuration:

```sh
gray discord share --skill /absolute/path/to/a-skill
gray discord share --context /absolute/path/to/nonsecret-memory.md
gray discord share --plugin-argv '["python3","-m","my_memory_plugin"]'
gray discord share --clear
```

Context is copied into the dedicated home prompt (128 KiB total); selected skill
directories are symlinked. Shared sidecars start per agent turn: do not select
another always-on gateway. `GRAY_SKILLS_ONLY=1` limits gray's conventional global
skill discovery. This is not an OS sandbox; selected code and the agent's shell
retain the service user's access. Restart the gateway after changing selections.

## Scheduling

`gray discord schedule add/list/remove` now operate while the gateway runs.
The SQLite schedule enqueue/advance is atomic. Legacy jobs.json is imported once
and left intact as a backup. Due jobs use the same worker/budget/outbox path as
chat. Missed intervals coalesce into one accepted job, not a catch-up burst.

The **plugin interval scheduler remains separate from gray's native cron**.
Unifying native cron claim/delivery semantics has not been implemented in this
branch; calendar-cron compatibility must not be implied. The native cron may
execute independently, so never register the same job in both.

## Verification and remaining integration work

Tests exercise the real gray binary with loopback model/Discord endpoints,
redacted-prompt resumption, conversation isolation, background delivery failure
and restart, a budget overage blocking the next run, cancellation, online schedule
edits, and selected capabilities. No user token/provider key is read by tests.
`owner_id` in the config is optional: absent means nobody is admitted yet,
and every human DM draws a pairing reply (their own ID plus a single-use
code) until `gray discord pairing approve discord <code>` lands — the bootstrap
pairing step. A running gateway re-reads approvals on restart.
Live Discord pairing now runs in setup itself: one short-lived gateway
connection waits for the owner's DM (see `setup::default_pairing`). The
service lifecycle runs under runit, systemd --user, or gray's own detached
spawn — install/status/stop/restart dispatch on what this box supervises
with. A live bot token and merging with the active gray checkout remain
operator steps.
