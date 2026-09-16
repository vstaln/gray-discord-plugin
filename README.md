# gray-discord-plugin

A standalone Discord plugin for [gray](https://github.com/vstaln/gray), with
its own setup wizard and background service. Public source, no Discord code
added to gray. Uses discord.py like Hermes and ports selected Hermes helpers
and behavior; see [attribution](THIRD_PARTY_NOTICES.md).

**Status: experimental text-only release.** Offline tests and real-gray
session integration pass. Live Discord login, interactive pairing, and
systemd deployment have not been exercised by the author in this release.

## Install and set up yourself

Requires a working Python 3.11+, gray on PATH, and gray's provider/model already
configured in `~/.gray/config.json`. Use a separate Discord bot application.

```sh
pipx install git+https://github.com/vstaln/gray-discord-plugin.git
gray-discord setup
gray-discord doctor
gray-discord install
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
gray-discord status
gray-discord restart
gray-discord stop
# Or run in the foreground without systemd:
gray-discord run
```

No inbound port is opened. Do not run a second instance with the same config.
Different configurations using the same token are not protected by this lock.

## Install the outgoing tool into gray

```sh
gray-discord register
```

This registers a protocol-1.1 stdio sidecar in gray's existing plugin lock,
preserving other entries. Restart existing gray sessions. `discord_send`
accepts `{ "content": "hello" }` and only sends to the configured destination;
it does not open a gateway connection. Its calls do not require the background
service to be running. Long messages are split, with all mentions suppressed.

**Current host limitation:** `gray discord setup` is not implemented in gray.
Use `gray-discord setup`. The host's Git importer extracts skills rather than
installing this Python executable; use pipx above, not `gray plugin install`.

## Scheduled messages

Plugin-owned interval jobs run the real gray agent and deliver its final reply
to your home channel. Stop the service to edit schedules (the command refuses
concurrent writes). Intervals are seconds, minimum 60.

```sh
gray-discord stop
gray-discord schedule add --every 3600 'Check the public project status and give a short update.'
gray-discord schedule list
gray-discord schedule remove JOB_ID
gray-discord restart
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

A per-conversation file lock prevents overlapping turns; the gateway admits at
most two chat turns plus one scheduled turn. Each child has a 600-second deadline;
timeout/shutdown kills its process group. This is not a dollar spending limit.
Only assistant text from the completed saved turn is delivered—no CLI stdout,
reasoning or tool output. Failed commands are not automatically retried.

This is conversation history, **not semantic long-term memory**. Existing desktop
skills/memories are not copied. Voice, attachments, slash-command UI, streaming
edits, startup message backfill and full Hermes parity are not included.

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
