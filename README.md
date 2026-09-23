# gray-discord-plugin

A standalone Discord plugin for [gray](https://github.com/vstaln/gray), with
its own setup wizard and background service. Public source, no Discord code
added to gray. Standalone compiled Rust binary using twilight and SQLite durable
queue; ports selected Hermes helpers and behavior; see [attribution](THIRD_PARTY_NOTICES.md).

**Status: standalone Rust binary (0.1.0).** Requires matching
gray core `--json` implementation.
See [runtime policy, commands and remaining limits](docs/RUNTIME.md).

**Experimental text-only integration.** Live Discord login, interactive pairing,
and background-service deployment have been ported from Python to a single
Rust binary. The service probes what this box actually supervises with
(runit, systemd --user, or gray itself with no init) instead of assuming
systemd.

## Installation

### Via gray catalog

```sh
gray install plugin discord
```

Prebuilt binaries are published for:
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

### From source

```sh
cargo build --release --locked
```

## Setup and Service Lifecycle

Requires gray on PATH, and gray's provider/model already
configured in `~/.gray/config.json`. Use a separate Discord bot application.

```sh
gray discord setup
gray discord status
```

Setup is one command (Hermes parity — token plus your user ID, nothing else):

```sh
gray discord setup
```

```
Discord bot token (hidden): ••••••••
Your Discord user ID (comma-separated to also allow others): 1493623750858375228,1502…
```

The wizard validates the token against Discord's own API, makes your DM the
home channel (created, never asked for), writes the config privately, and
runs the doctor. Your first ID is the owner; the rest join the allowlist.
Enable Message Content Intent in the Developer Portal (the doctor checks it).
Don't know your user ID? `gray discord setup --pair` instead prints a
one-time code, you DM it to the bot, and the wizard discovers your ID and DM
channel from that DM.

**Pairing happens on Discord too.** Once the bot is running, anyone can DM
it: an unconfigured human is told their own Discord ID and a pairing code,
and the owner admits them with one command:

```
access not configured.
Your Discord user id: 1493623750858375228
Pairing code: SNU3ZQ37
Ask the bot owner to approve with:
gray discord pairing approve discord SNU3ZQ37
```

Codes are single-use; approving before an owner exists makes that user the
owner. A running gateway applies approvals on restart.

The wizard never sends credentials to a model. Private config is written
atomically with mode 600. Do not paste bot tokens into chat or command arguments.
To use a nondefault config, place `--config /absolute/path/config.json` before
the command on every invocation, including setup/install/register.

`gray discord install` starts the daemon under whatever this box supervises
with. On a systemd box the **user** unit is `gray-discord-plugin.service`;
for operation after logout/reboot, enable user lingering if permitted:

```sh
loginctl enable-linger "$USER"
```

On a runit box the service is written to `~/.config/service/gray-discord/run`
and brought up with `sv`. With no init at all, gray spawns the daemon
detached with a pidfile and log beside the config. `gray discord status`,
`restart`, and `stop` all dispatch to whichever is installed:

```sh
gray discord status
gray discord restart
gray discord stop
# Or run in the foreground under any supervisor:
gray discord run
```

No inbound port is opened. Do not run a second instance with the same config.
Different configurations using the same token are not protected by this lock.

## Outgoing tool registration

```sh
gray discord register
```

Setup does this automatically; the command above can repeat it. It registers a
protocol-1.1 stdio sidecar in gray's existing plugin lock, preserving other entries.
Restart existing gray sessions. `discord_send` accepts `{ "content": "hello" }`
and only sends to the configured destination; it does not open a gateway connection.
Its calls do not require the background service to be running. Long messages are
split, with all mentions suppressed.

## Burst guard and attachments

Two ports from the grayai_legacy bot (Python, `vstaln/grayai_legacy`), kept
minimal:

- `rate_limit_capacity` + `rate_limit_window_secs` (8 / 60s defaults) — a
  sliding-window guard per user. Without it one eager DMer is twenty
  concurrent `gray` processes; the ninth message inside the window gets
  "Too fast — try again in Ns" instead of a fork.
- `max_attachment_bytes` (8MB default) — DM attachments are saved under
  `workdir/attachments/<msg-id>-<name>` and named in the prompt
  (`[attached file: …]`), so gray's own tools read them: `cat` for text,
  `cat <image>` for vision. Nothing is decoded here.
- Outbound text is sanitized before Discord renders it (bare links wrapped
  so they do not become embed cards, `:fire:` names replaced, doubled
  heading hashes collapsed) — a port of `services/text_sanitizer.py`.

## Typing indicator

On by default, and re-poked every 8 seconds while the agent is working — so
from Discord it reads as a permanent "typing…" bubble on a long turn. Turn
it off in `config.json`:

```json
{"typing_indicator": false}
```

Same key, default, and gate placement as Hermes' `discord.typing_indicator`:
the check happens in the adapter before any typing call, so `false` stops
the whole path (the REST poke and the host hook) rather than one loop of it.
Reload by restarting the service (`gray discord restart`).

## Activity narration

While the agent works, one status message per channel shows what it is
doing, overwritten in place as the turn proceeds — Hermes' single-bubble
model, not one message per tool call:

```
💻 terminal: cargo test -p gray
📖 Reading config.yaml L110-139
🧠 the user wants magic words
```

The rows come from gray core's `--json` progress stream (phase + tool +
a redacted one-line detail), so every chat surface can render the same
narration; only the rendering is Discord-specific. Reasoning traces
appear when gray is configured to show them (`GRAY_SHOW_REASONING`, which
the runner sets from this switch). Every disclosed detail is redacted and
capped before it leaves gray.

Off in `config.json`:

```json
{"activity_indicator": false}
```

Absent means on. Turning it off also stops reasoning from being streamed
to the runner. Reload by restarting the service (`gray discord restart`).

## Cron that comes back to the chat

Ask for a reminder in Discord ("remind me to check the deploy every hour")
and the job posts its result back to the channel it was added from:

```
Cronjob Response: check the deploy
(job_id: 125c6c57422c)
-------------

the deploy is green

To stop or manage this job, send me a new message (e.g. "stop reminder check the deploy").
```

The frame is gray core's, byte-for-byte Hermes' `_deliver_result`; this
plugin only carries it. How it fits together:

- Each turn runs in its own gray home, so a job added from a conversation
  lives in that conversation's store and fires with its credentials,
  workdir, and skills.
- The channel binding is a `route.json` next to that store, written where
  the channel is known and read where the home is known, so a plain
  `gray cron add` from the model is already bound — no ids in the prompt.
- A background task ticks each conversation every 60s via
  `gray cron tick --json` and posts what comes back. Runs as its own task,
  so a firing never stalls shard events.
- Output that is `[SILENT]` is not posted (core suppresses it), and the
  full transcript of every run stays on disk.

## Allowlisted users

Allow additional users to trigger the agent (usage is billed to the owner's budget ledger):

```sh
gray discord allowlist add <USER_SNOWFLAKE_ID>
gray discord allowlist list
gray discord allowlist remove <USER_SNOWFLAKE_ID>
```

Empty allowlist means owner-only access.

## Scheduled messages

Plugin-owned interval jobs run the real gray agent and deliver its final reply
to your home channel. Schedule edits are transactional and work while the service
is running. Intervals are seconds, minimum 60.

```sh
gray discord schedule add --every 3600 'Check the public project status and give a short update.'
gray discord schedule list
gray discord schedule remove JOB_ID
```

Jobs persist next-run time and `scheduled/running/sent/failed` status in SQLite.
The next run advances before execution; interrupted jobs are not replayed. These
are interval jobs, not cron expressions and not gray's native cron scheduler.
Avoid calling `discord_send` in job prompts: the scheduler already sends the reply.

## Slash commands

The bridge answers native Discord slash commands. Registration, dispatch and
`/help` are all derived from one table (`src/commands.rs`), so a command that
exists is registered, handled and documented or none of the three.

| Command | What it does | Visibility |
|---|---|---|
| `/help [command]` | Lists every command, or one in detail | public |
| `/ask prompt:…` | Enqueues a turn for gray | public |
| `/cron list` | This channel's scheduled jobs, with a Remove button each | public |
| `/cron add every:30m prompt:…` | Schedules a prompt **for the channel it is typed in** | public |
| `/cron remove id:…` | Deletes a job | private |
| `/model` | Which model answers here, and where it comes from | public |
| `/model set model:…` | Pins a model for this channel only | public |
| `/memory list\|show\|set\|remove` | gray's curated cross-session memory | list/show/set public, remove private |
| `/status` | Queue depth, model, bridge version | public |
| `/reset` | Forgets your session | private |
| `/stop` | Cancels the running turn | private |

Anything that shells out to gray (`/memory`) acknowledges first and answers
afterwards: Discord drops a callback that took longer than three seconds.
Buttons arrive as `MessageComponent` interactions; only an allow-listed user
can press one, because Discord does not filter presses for you.

`/cron` fronts the plugin's own schedule store rather than gray's cron CLI.
gray's cron is file-only and needs a ticker per home; this daemon ticks only
its own table, so a `gray cron add` issued from Discord would never fire.
Schedules therefore carry the channel and conversation they belong to, and a
job created in one channel delivers to that channel — a job added by
`gray discord schedule add` targets the configured home channel.

## Sessions and safety boundaries

Each conversation and each job gets its own private gray home, provider config
snapshot, sessions and working directory below the plugin config directory.
Turns resume the existing ID; ambiguous session stores fail rather than guessing.
A gray profile explicitly enables `tools-minimal` and the outgoing sidecar.
Tools run on the server, not your desktop. This is **not a sandbox**: allowlisted
users and model tool execution have the OS user's permissions. Prefer a dedicated
unprivileged service account; do not give it broad SSH/cloud credentials.

Accepted messages and outgoing replies are persisted in a transactional SQLite queue.
Agent execution and delivery retries are separate; uncertain interrupted work
is never automatically rerun. Deadlines/concurrency/model-call limits are
configurable, and the gateway requires explicit model prices and spending
allowances. These are client accounting controls, not provider invoice guarantees.
The runner reads structured results rather than searching session JSONL.

Use `gray discord share` for explicitly selected skills, non-secret context and
memory sidecars. Conversation histories remain isolated. Large compacted histories
are archived without removing the original. See [runtime details](docs/RUNTIME.md)
for exactly-once delivery limitations, unbounded archive retention, and the
still-separate native cron scheduler. Voice, attachments and full Hermes parity
are not included.

`doctor` checks token, intent, channel permissions and local gray configuration;
it does not test provider generation or prove gateway connectivity.
`uninstall` removes the service only; config, sessions, jobs and the registered
outgoing tool are deliberately retained. Use `gray plugin disable discord` to
turn off that tool. Remove private state yourself only if you want to erase it.

## Development / verification

```sh
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

In CI or headless environments:
```sh
cargo test --locked --all-targets
```
