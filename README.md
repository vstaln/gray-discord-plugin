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

Setup reads the bot token with hidden terminal input, validates it against
Discord's own API, prints an invite link, then connects one short-lived
gateway and asks you to DM a one-time code to the bot. The code expires in
five minutes. Confirm the resulting user ID locally before configuration is
saved. Only that account and explicitly allowlisted users can trigger the agent.
Enable Message Content Intent in the Discord developer portal. Pick a home
channel (your DM by default). The spend budget is optional: decline and the
daemon runs with no ledger; `gray discord budget set` opts in later.

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
