# Runtime reliability changes (isolated development version)

Requires the matching gray core changes implementing `gray -p --json` protocol 1.
Do not install this branch with an older gray executable. No active service was
updated or started as part of this work.

## Structured execution

The runner consumes NDJSON, not terminal output or session-file text searches.
Progress contains phase names only. The terminal result includes `session_id`,
`turn_id`, final redacted assistant text, and provider accounting. Errors are
sanitized and retain a nonzero exit status. Malformed output is rejected.
Each conversation keeps an explicit session pointer. Existing single-file
conversation homes migrate on first use; ambiguous stores fail closed.

`gray discord limits --timeout-seconds 1800 --concurrency 4 --max-requests 32`
sets runtime policy; restart to apply. Defaults: 600 seconds, 2 workers, 32
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
Live Discord pairing, real systemd startup, and merging with the active gray
checkout remain separate operator/integration steps.
