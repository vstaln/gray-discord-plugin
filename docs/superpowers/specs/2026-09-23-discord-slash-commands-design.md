# gray-discord slash commands, embeds, buttons — design spec

- Date: 2026-09-23. Status: user-approved (chat, 2026-09-23).
- Goal: the Discord bot exposes gray's command surface as **native slash
  commands**, answered with **embeds** and a few **buttons**, so a user who
  knows the REPL finds the same verbs in a channel.
- Non-goals: voice/media, Discord Activities/games, modal text forms
  (phase 2), self-hosted activities, any gray-core change, replacing `/ask`,
  giving the bot more than bash+skills inside a turn.

## §0 What already exists (verified, do not rebuild)

- `gateway.rs:28` `slash_commands_json()` registers 4 commands:
  `ask` (one required `prompt` option), `reset`, `status`, `stop`.
- Dispatch is one `match cmd.name.as_str()` inside the
  `Event::InteractionCreate` arm (`gateway.rs` ~695), already doing:
  allow-list gate (`policy::is_allowed_user`), `defer` type 5 for slow
  work, `interaction_callback` type 4 + `flags: 64` (ephemeral) for fast
  work, and `store.set_interaction` so the outbox can answer via followup.
- Registration is hash-cached in `meta` (`slash_commands_hash`), bulk PUT
  via `transport::overwrite`-style global commands.
- `transport::send_embed` (REST embeds) exists but is used only for the
  pairing DM. `transport::followup(app_id, token, text)` is text-only.
- `durable::Meta` (`meta_get`/`meta_set`) persists plugin key/values.
- Runner (`runner.rs` ~215) spawns
  `gray -p <prompt> --json --max-requests N [--session SID]` with
  `GRAY_HOME={config dir}/conversations/{sha256(conversation)}`, so every
  channel has its own gray home. `gray -p` accepts `--model`
  (gray-core `lib.rs` `Cli`).
- gray's CLI owns: `cron list|add|remove|pause|resume|show|run`,
  `memory [--scope] list|show|set|edit|remove|clear|audit`.
- The plugin owns its own `schedules` table (interval-only, fires via
  `enqueue_due` in the daemon tick) and its `inbox`/`outbox` queue.

## §1 Architecture

New module `src/commands.rs` holding ONE table of command specs:

    pub struct Spec { name, description, subcommands: &[SubSpec], public: bool }

`slash_commands_json()` renders the Discord registration JSON from that
table, the dispatcher looks a handler up from that table, and `/help`
renders its embed from that table. **Three readers, one source of truth**
— a command that ships unregistered or undocumented is a compile-time
hole, not a runtime surprise.

Handler signature (sync where possible; the async ones run the CLI):

    async fn handle(ctx: &Ctx) -> CommandReply
    enum CommandReply { Embed { embed: Value, public: bool },
                        Files  { embed: Value, components: Vec<Value>, public: bool },
                        Text(String) }

All handlers answer through one reply helper that owns the
`interaction_callback` (type 4) / `followup` choice, so no handler touches
Discord JSON.

## §2 Dispatch and interaction plumbing

Extend the `match cmd.name.as_str()` block; each arm is a one-line lookup
into the table. Two additions:

1. **`components` on replies** — buttons arrive as
   `InteractionData::MessageComponent`, a branch that does NOT exist today.
   Parse `custom_id` (`cron:remove:<id>`, `model:set:<id>`), re-run the
   same allow-list gate, and answer through the same reply helper. Only
   owner-allowed users can press (Discord does not filter for us).
2. **`followup_embed`** — `transport::followup` is text-only; add an
   embed sibling bounded like `send_embed` (6000). `defer` stays type 5
   for anything touching the CLI.

## §3 Where each command's state lives

Deliberately not uniform, driven by *who owns the runtime behaviour*:

- **`/cron` → the plugin's own `schedules` table.** This is the one
  correction to the chat-approved "dispatch to gray's CLI": gray's cron is
  file-only and needs a ticker per home, and this daemon ticks only its
  own table, so a `gray cron add` would silently never fire. `/cron`
  becomes the embed front-end for the scheduler that actually runs:
  `list`, `add --every <s> --prompt <text>`, `remove`. Requires
  **conversation-scoped schedules** (today `enqueue_due` delivers every job
  to the configured home channel): add a `conversation TEXT` column with an
  `ALTER TABLE` migration matching the existing
  `interaction_token`/`app_id` pattern, store `chat:<channel>` on add, and
  deliver to it.
- **`/memory` → `gray memory …` with the conversation's `GRAY_HOME`.**
  Pure files, no ticker, and gray stays the single source of truth for the
  format the agent itself reads. `list`, `show <key>`, `set <key> <text>`,
  `remove <key>`.
- **`/model` → the plugin.** No gray-core change needed: stash the pick in
  `meta` (`model:<conversation>`), and the runner appends `--model` to the
  argv it already builds when the key is set. Display-only when called with
  no argument; with a `set` subcommand it also offers the last few as
  buttons.
- **`/status` → rewrite** the existing plain-text reply as an embed
  (queue depth, per-conversation state, version, current model).

## §4 Visibility

Informational commands (`help`, `status`, `cron list`, `memory list`,
`model`) post **publicly** so the channel reads as the shared surface;
destructive ones (`cron remove`, `memory remove`, `model set`'s confirm
mental model) stay **ephemeral** (`flags: 64`). `/help` is the reference
people screenshot.

## §5 Embeds

Shared renderer in `commands.rs`: title, description (markdown allowed),
fields, footer. Rules: never an embed for plain chat answers (the agent's
text already renders fine), never more than 4096/256/6000 characters —
truncate with a pointer, never silently. `/help` groups by command with
the subcommands listed, so adding a command needs no `/help` edit.

## Acceptance

- `/help`, `/cron`, `/model`, `/memory`, `/status` registered, dispatched,
  documented from one table; `cargo test` proves help/registry parity.
- Every reply is an embed except `/ask`; buttons fire on `cron` rows.
- A `/cron add` in channel B actually delivers to channel B (migration
  covered by a test).
- `gray memory`/`gray cron` shells run with the conversation's `GRAY_HOME`
  and are unit-tested against a temp home, not the live one.
- `cargo test --all-targets`, `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings` clean.
