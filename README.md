# gray-discord-plugin

A standalone Discord plugin for [gray](https://github.com/vstaln/gray), with
its own setup wizard and background service. Public source, no Discord code
added to gray. Uses discord.py like Hermes and ports selected Hermes helpers
and behavior; see [attribution](THIRD_PARTY_NOTICES.md).

**Status: isolated runtime-reliability development branch.** Requires the matching
gray core `--json` implementation; do not upgrade an active service independently.
See [runtime policy, commands and remaining limits](docs/RUNTIME.md).

**Experimental text-only integration.** Offline tests and real-gray
session integration pass. Live Discord login, interactive pairing, and
systemd deployment have not been exercised by the author in this release.

## Rust binary (0.2.0)

`gray-discord` 0.2.0 is a standalone Rust binary port of this plugin, replacing the Python runtime with a single compiled binary, zero Python runtime dependencies, an asynchronous twilight-based Discord gateway, SQLite durable queue with WAL mode, and complete parity with the original Hermes-inspired design.

### Installation via gray

```sh
gray install plugin discord
```

Prebuilt binaries are published for:
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

### Configuration reuse & migration

Existing `~/.config/gray-discord/config.json` files are fully forward-compatible. The Rust binary reads existing configuration seamlessly. `allowed_users` defaults to `[]` when absent. Existing session files and `jobs.json` (migrated to `queue.sqlite` schedules) are preserved.

### Python deprecation note

The Python implementation (`gray_discord`) is deprecated and will be removed in release 0.2.0 once binary distribution is active. All new features, performance improvements, and security enhancements are developed exclusively in the Rust binary.

## Install and set up yourself (Python legacy)

Requires a working Python 3.11+, gray on PATH, and gray's provider/model already
configured in `~/.gray/config.json`. Use a separate Discord bot application.

```sh
gray install plugin discord
gray discord setup
gray discord status
```

Setup reads the bot token with hidden terminal input, validates it, prints an
invite link, then asks you to DM a one-time code to the bot. The code expires
in five minutes. Confirm the resulting user ID locally before configuration
is saved. Only that account can trigger the agent. Enable Message Content
Intent in the Discord developer portal. Pick a home channel (your DM by default).

The wizard never sends credentials to a model. Private config is written
atomically with mode 600. Do not paste bot tokens into chat or command arguments.
To use a nondefault config, place `--config /absolute/path/config.json` before
the command on every invocation, including setup/install/register.

The systemd **user** service is `gray-discord-plugin.service`. It uses the
Python environment where the plugin is installed. Keep that environment.
For operation after logout/reboot, enable user lingering if permitted:

```sh
loginctl enable-linger "$USER"
gray discord status
gray discord restart
gray discord stop
# Or run in the foreground without systemd:
gray discord run
```

No inbound port is opened. Do not run a second instance with the same config.
Different configurations using the same token are not protected by this lock.

## Outgoing tool registration

```sh
gray discord register
```

Setup does this automatically; the command above can repeat it. It registers a protocol-1.1 stdio sidecar in gray's existing plugin lock,
preserving other entries. Restart existing gray sessions. `discord_send`
accepts `{ "content": "hello" }` and only sends to the configured destination;
it does not open a gateway connection. Its calls do not require the background
service to be running. Long messages are split, with all mentions suppressed.

Requires a gray build with catalog installation and plugin command dispatch.
Older releases that reject `gray install` must be upgraded first. Gray creates
a private venv and installs the catalog's pinned source; Python's venv/pip support
and Git must be available. No global Python packages or separate CLI are required.
Setup automatically registers the outgoing tool and offers to enable/start the
background service. Declining service startup leaves foreground use available.

## Scheduled messages

Plugin-owned interval jobs run the real gray agent and deliver its final reply
to your home channel. Schedule edits are transactional and work while the service
is running. Intervals are seconds, minimum 60.

```sh
gray discord schedule add --every 3600 'Check the public project status and give a short update.'
gray discord schedule list
gray discord schedule remove JOB_ID
```

Jobs persist next-run time and `scheduled/running/sent/failed` status. The
next run advances before execution; interrupted jobs are not replayed. These
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

Accepted messages and outgoing replies are persisted in a transactional queue.
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
python3 -m venv .venv
.venv/bin/pip install -e .
.venv/bin/python -m unittest discover -s tests -v
GRAY_TEST_BIN=/absolute/path/to/gray .venv/bin/python -m unittest discover -s tests -v
.venv/bin/pip wheel --no-deps . -w dist
```

Without `GRAY_TEST_BIN`, the real-gray integration test is explicitly skipped.
With it, a loopback scripted provider captures real gray requests to verify
second-turn history replay and cross-conversation isolation—no API key, external
model, or Discord token required. No tests read your actual provider credentials.
