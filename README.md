# gray-discord-plugin

A standalone Discord plugin for [gray](https://github.com/vstaln/gray), with
its own setup wizard and background service. Public source, no Discord code
added to gray. Standalone compiled Rust binary using twilight and SQLite durable
queue; ports selected Hermes helpers and behavior; see [attribution](THIRD_PARTY_NOTICES.md).

**Status: standalone Rust binary (0.1.0).** Requires matching
gray core `--json` implementation.
See [runtime policy, commands and remaining limits](docs/RUNTIME.md).

**Typed Components V2 integration.** Live Discord login, interactive pairing,
background-service deployment, managed media, and typed component events are
implemented in one Rust binary. The service probes what this box actually
supervises with (runit, systemd --user, or gray itself with no init) instead
of assuming systemd.

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
split, with all mentions suppressed. New bot messages use Discord Components V2
(`Text Display`, `Container`, and `Separator`) rather than legacy content/embeds;
the durable queue keeps a legacy fallback only for rows created before the V2
migration.

## Typed Components V2

The sidecar exposes a Gray-owned typed document protocol instead of asking the
model to hand-write Discord numeric types or `custom_id` values:

- `discord_ui_schema` returns every component's fields, Discord's limits,
  design tips, and copy-ready examples (status card, image post, item with
  thumbnail, button choice).
- `discord_send_ui` accepts a `gray.discord.ui` document, compiles it,
  allocates opaque state, and sends/edits a V2 message.
- `discord_open_modal` accepts a typed modal document for the current interaction.
- `discord_file` posts local files straight to the channel (`action: "send"`,
  images in one gallery, other files as file cards, optional `caption`), or
  imports one privately and returns a `file_id` for `discord_send_ui` media.
  Any readable path works (absolute, `~/`, or relative to the session's working
  directory) except credential and config files (`.ssh`, `.env`, `*.pem`,
  Gray's own state, ...).

### Forgiving input, exact errors

Documents are normalized before the strict types see them, so the shapes models
naturally write all work: Discord's own numeric JSON (`{"type": 17, ...}`),
`text_display`, a bare list of components, a JSON string, bare URLs or
`file_id` strings as media, `#RRGGBB` colours, bare buttons (grouped into rows
automatically), and top-level `title`/`accent_color` shortcuts that wrap the
message in one accented card. `version`, `document_id`, and `surface` are
filled in when missing.

Anything still wrong comes back as `Not sent: invalid document: $.components[0].components[2] (type text): unknown field ...`
before any request is made. If Discord itself rejects a payload, its
validation paths are returned too, e.g.
`Discord rejected the request (HTTP 400): Invalid Form Body: components[0].spacing: ...`.

### Hermes-style `MEDIA:` tags

Like Hermes, the agent can attach files to any reply by writing
`MEDIA:/absolute/path/to/file.png` in its answer (or in `discord_send`). The tag
is stripped, and the files are uploaded after the text as a Components V2
message: images and videos in one media gallery, everything else as file
cards, ten per message. Tags that name a missing or blocked file are left in
the text so nothing disappears silently.

By default any readable file can be sent except credentials and config. To
lock this down, list the folders files may come from in `config.json`:

```json
{"media_roots": ["~/gray", "/tmp"]}
```

With `media_roots` set, `MEDIA:` tags, `discord_send`, and `discord_file`
refuse anything outside those folders (and the credential rules still apply).

Buttons, selects, modals, media, files, and the documented component limits are
handled by the plugin. Interaction values are delivered into Gray as
`gray.discord.input` version-1 user turns. Premium buttons require the
`premium` capability and an explicitly allowed `premium_skus` entry.

A message document is authored in the Gray protocol:

```json
{
  "components": [
    {"type": "container", "accent_color": "#57F287", "components": [
      {"type": "text", "content": "## Build finished\n-# main · 3m 12s"},
      {"type": "separator"},
      {"type": "section", "children": ["**42** tests passed"],
       "accessory": {"type": "thumbnail", "media": "https://example.com/badge.png"}},
      {"type": "action_row", "children": [
        {"type": "button", "logical_id": "refresh", "label": "Refresh", "style": "secondary"},
        {"type": "button", "label": "Logs", "style": "link", "url": "https://example.com/logs"}
      ]}
    ]}
  ]
}
```

Call it through `discord_send_ui`. For a modal, use the same envelope with
`"surface": "modal"`, a `title`, and `label` components, then call
`discord_open_modal` with the interaction ID from the normalized event.
`discord_file` supports `send`, `import`, `metadata`, and `path`; only `path`
returns a file below the current conversation work directory.

The compiler follows Discord's component reference: separator spacing is sent
as `1`/`2`, media items carry only `url`, File components accept only uploaded
files, thumbnails appear only as section accessories, all text in one message
totals at most 4000 characters, and `required` is sent on selects only in
modals. It emits message types 1–3, 5–14, and 17, plus modal types 3–8,
18–19, and 21–23. Numeric Discord types, custom IDs, and attachment URLs are
owned by the compiler. Premium style 6 is represented in the protocol but
fails closed unless both the capability and SKU allowlist are configured.

## Native structured input

The gateway and sidecar never put Discord interaction tokens or raw callback
bodies in a model prompt. A normalized event is persisted as a versioned Gray
input envelope and runs in the same conversation/session as the user's text:

```sh
gray --input-json /path/to/event.json --json --session <id>
gray --input-json - --json --session <id> < event.json
```

`event.json` has the shape:

```json
{
  "protocol": "gray.discord.input",
  "version": 1,
  "kind": "component_event",
  "payload": {
    "document_id": "refresh-card",
    "component": "refresh",
    "action": "button",
    "values": {"id": "7"},
    "files": []
  }
}
```

The envelope is capped at 1 MiB and is preserved as a typed user content block
in the transcript. The provider sees a bounded, redacted marker, not a raw
Discord token or file body.

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

## Live replies and activity narration

Each turn is one live Components V2 card, edited in place while the agent
works:

````markdown
┃ Let me check what's running on the box.
┃ -# 💻 Ran `gray ps` (0.3s)
┃ **Done / idle:** claude-sub plugin agent: done. PR #173 is ▉
┃ ───────────────────────────────────────────────
┃ -# ⏳ working · 20s · ran 1 command          [⏹️ Stop]
````

- The card is posted as soon as there is something to show. The answer then
  streams into it about once a second with a ` ▉` cursor.
- Prose and tool lines keep their order: each run of prose, then the tool
  lines it led to (small grey subtext), then the next prose.
- The footer shows status, time and a tally. Its clock moves in 10-second
  steps, so an idle turn costs one edit per step.
- **Stop** (danger button) stops the turn, like `/stop`. Any admitted user
  in the channel can press it once.
- The accent bar tracks the turn: blurple while working, green when done
  (`✅ done in 7.2s · ran 1 command`), red on failure, grey when stopped
  (`⏹️ stopped after 12s · actions may already have happened`). A failed or
  stopped card is the turn's notice, so nothing extra is posted.
- A long turn continues in further cards (Discord allows 40 components and
  4000 characters per message), cut at a newline, with code fences closed
  and reopened at the cut, up to 8 cards. Only the last card has the footer.

The final edit is the authoritative answer, recorded in the durable outbox
as delivered, so a restart never posts it twice. `MEDIA:` tags are hidden
from the preview and their files are uploaded after the card. If the answer
cannot land in place (a refused post, a deleted card), the preview is
retracted and the answer is posted durably instead. Slash command turns
answer through their interaction; their card shows tool lines only.

Streaming needs a gray core that emits `text` rows (`GRAY_STREAM_TEXT=1`,
set by the bridge). With an older gray the bridge falls back to posting the
whole answer at the end. Off in `config.json`:

```json
{"stream_replies": false}
```

Tool lines show actions only: one line per meaningful call, never tool
output. Shell introspection (`ls`, `cat`, …) stays unnarrated, and shell
work reads the way gray's own transcript labels it (`Running` while it
runs, `Ran` with its duration once it returns). The card stays in the
channel as the turn's record.

The rows come from gray core's `--json` progress stream (phase + tool +
detail, plus the streamed prose), so every chat surface can render the same
data; only the presentation is Discord-specific. Raw model reasoning is
never sent to Discord: the bridge always runs gray with
`GRAY_SHOW_REASONING=0`. Secrets are redacted before anything leaves gray
(streamed prose holds back the word still being typed, so a key is never
shown half written), but secret-free paths stay verbatim so narration names
the actual file.

Tool lines off (the reply still streams into its card):

```json
{"activity_indicator": false}
```

The old end-of-turn tally card (`⋯ 0.9s · ran 2 commands` plus up to 5
actions) is opt-in with `{"activity_card": true}`. Reload any of these by
restarting the service (`gray discord restart`).

## Sessions and new conversations

A DM is one conversation. A native Discord thread is already a separate
conversation because Discord sends that thread's channel ID. Guild messages
are isolated per user, so two people in one channel do not share a transcript.

By default Gray follows the local Hermes policy: a session is replaced after
24 hours idle or at the next 04:00 local boundary, whichever comes first.
The policy is configurable:

```json
{
  "session_reset": {
    "mode": "both",
    "idle_minutes": 1440,
    "at_hour": 4
  }
}
```

`mode` may be `none`, `idle`, `daily`, or `both`. `/new` and `/reset` both
forget the caller's transcript, cancel work already queued for that
conversation, and leave a fresh generation marker so an old in-flight turn
cannot restore its previous session.

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
| `/new` | Starts a fresh session for you | private |
| `/reset` | Alias of `/new` | private |
| `/stop` | Cancels the running turn | private |

Anything that shells out to gray (`/memory`) acknowledges first and answers
afterwards: Discord drops a callback that took longer than three seconds.
Buttons arrive as `MessageComponent` interactions; only an allow-listed user
can press one, because Discord does not filter presses for you. Cron buttons
carry short-lived, opaque, single-use state tokens rather than schedule IDs, and
successful presses update the original V2 message.

`/cron` fronts the plugin's own schedule store rather than gray's cron CLI.
gray's cron is file-only and needs a ticker per home; this daemon ticks only
its own table, so a `gray cron add` issued from Discord would never fire.
Schedules therefore carry the channel and conversation they belong to, and a
job created in one channel delivers to that channel — a job added by
`gray discord schedule add` targets the configured home channel.

## Sessions and safety boundaries

Each conversation and each job gets its own private gray home, provider config
snapshot, sessions and working directory below the plugin config directory.
Turns resume the existing ID until the reset policy or `/new` starts a new one;
ambiguous session stores fail rather than guessing. Native Discord threads use
their own channel key, and guild chats use a per-user key. Resets remove the
private conversation transcript rather than merely deleting its pointer.
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

`doctor` checks token, intent, effective channel/parent overwrite permissions
(including the thread send bit), and local gray configuration; it does not test
provider generation or prove gateway connectivity.
`uninstall` removes the service only; config, sessions, jobs and the registered
outgoing tool are deliberately retained. Use `gray plugin disable discord` to
turn off that tool. Use `/new` or `/reset` to erase the current conversation;
remove other private state yourself only if you want to erase it.

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
