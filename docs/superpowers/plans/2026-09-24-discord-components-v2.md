# Exhaustive Discord Components V2 and Typed Interaction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Gray Discord a complete typed Components V2 surface for every documented message/modal component, media/file lifecycle, and stateful button/select/modal/autocomplete input routed into the same Gray session.

**Architecture:** Gray core adds a versioned structured-input envelope and a typed `ContentBlock` variant while preserving ordinary text prompts. The standalone plugin owns a Gray-owned typed UI protocol, compiles it to validated Discord JSON, stores opaque component state in SQLite, delivers every interaction as a typed conversation event, and performs all Discord transport and media work. Plugin code and agent code use the same protocol with separate trust origins; the agent never supplies raw Discord IDs or JSON.

**Tech Stack:** Rust 1.89, Tokio, serde/serde_json, reqwest 0.12 with JSON/rustls/multipart, rusqlite bundled, twilight-gateway/twilight-model 0.17, Gray core/provider/CLI, loopback HTTP fixtures, tempfile.

**Spec:** `/home/vstaln/grayplugins/gray-discord-plugin/docs/superpowers/specs/2026-09-24-discord-components-v2-design.md`

## Global Constraints

- Work on the existing branch `feat/pairing-and-service-lifecycle`; do not create a second branch or a second PR.
- Keep the existing `discord_send` `{ "content": "..." }` contract working; it compiles to a V2 Text Display.
- Never accept raw Discord component JSON, model-selected `custom_id` values, arbitrary callback URLs, or arbitrary host paths.
- The Gray-level UI protocol is versioned; Discord numeric component types are emitted only by the compiler.
- Cover all 20 currently documented component type numbers: 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 17, 18, 19, 21, 22, and 23.
- New message requests use `IS_COMPONENTS_V2` (32768) and clear legacy `content`/`embeds`; legacy queue rows may drain through the old transport.
- Interaction acknowledgements are sent only after the typed event is durably recorded and must fit Discord's three-second budget.
- Interactive state is bound to document, revision, user, guild/channel, message/modal, and expiry; Discord interaction IDs are idempotency keys.
- Agent-authored components emit typed events to Gray. Plugin-authored components use the same event path; the plugin never executes a hidden action side effect.
- Premium button style 6 is represented but fails closed unless a separately configured SKU capability is enabled.
- Managed files are private, bounded, hash-identified, and exposed to Gray only through stable file IDs or a safe conversation-workdir path returned by the plugin tool.
- Never log tokens, raw Discord bodies, file contents, secret-shaped event values, or provider responses.
- Use `CARGO_BUILD_JOBS=4`; run narrow crate tests while iterating, then full plugin/core verification once before integration.
- Commit only the exact files named by each task; never use `git add -A` and never push or merge without explicit approval.

## File Map

The work is sequential rather than four independent products: core input storage is consumed by the runner, the protocol is consumed by state/transport, and state/transport are consumed by Gateway and sidecar integration.

### Gray core repository

- Create `crates/gray-core/src/input.rs`: versioned structured-input envelope, validation, safe size limits, and conversion to a message content block.
- Modify `crates/gray-core/src/lib.rs`: export the input contract.
- Modify `crates/gray-core/src/message.rs`: add `ContentBlock::StructuredInput` while preserving ordinary text/image/tool blocks.
- Modify `crates/gray-core/src/redaction.rs`: recursively redact structured event values before persistence/logging.
- Modify `crates/gray-core/src/agent_compact.rs` and `crates/gray-core/src/compact.rs`: count, display, and preserve structured events without losing IDs or event kind.
- Modify `crates/gray-provider/src/openai.rs`: render the structured block as a bounded, marked provider input part in both Chat Completions and Responses paths.
- Modify `crates/gray-core/src/agent_loop.rs`, `crates/gray-core/src/agent_tools.rs`, and `crates/gray/src/repl/format.rs`: update exhaustive content-block matches and terminal/transcript display.
- Modify `crates/gray/src/lib.rs`: add `--input-json PATH` with a mutually exclusive one-shot input group.
- Modify `crates/gray/src/main.rs`: read a file/stdin envelope and dispatch structured print mode.
- Modify `crates/gray/src/print.rs`: factor one-shot message construction so text and structured inputs share session/JSON/persistence behavior.
- Create `crates/gray-core/src/input_tests.rs` and `crates/gray/src/input_json_tests.rs`; wire both test modules from their crate roots and extend provider/redaction/compaction tests.

### Gray Discord plugin repository

- Create `src/component_protocol.rs`: typed Gray UI document, message/modal node enums, logical IDs, capabilities, and serde validation.
- Create `src/component_compile.rs`: protocol-to-Discord compiler, all Discord limits, opaque ID allocation, and Discord wire JSON.
- Create `src/component_state.rs`: durable documents, opaque states, event idempotency, expiry, and revisions.
- Create `src/component_media.rs`: managed file registry, import/download, hashes, limits, and multipart descriptors.
- Create `src/component_input.rs`: normalized button/select/modal/autocomplete events and Gray input envelope creation.
- Modify `src/render.rs`: retain trusted text/card/activity adapters, but route agent/plugin documents through the typed protocol/compiler.
- Modify `src/durable.rs`: schema migrations, typed inbox input, and component table access helpers.
- Modify `src/transport.rs`: compiled V2 sends/edits, all interaction response types, multipart uploads, and file references.
- Modify `src/gateway.rs`: normalize every Discord interaction, resolve state, enqueue typed events, and choose response type 4/5/6/7/8/9.
- Modify `src/command_dispatch.rs`: keep legacy slash commands, replace raw custom-ID button routing with the typed event path, and expose safe command/modal helpers.
- Modify `src/activity.rs`, `src/commands.rs`, `src/cron.rs`, and `src/pairing.rs`: compile all new output through typed documents.
- Modify `src/sidecar.rs`: extend the manifest and `discord_send` with typed document operations and safe file retrieval.
- Modify `src/runner.rs`: add structured turn input, secure temporary input files, and native `gray --input-json` invocation.
- Modify `src/lib.rs`: register the new modules.
- Extend `tests/components.rs`, `tests/delivery.rs`, `tests/durable.rs`, `tests/activity.rs`, `tests/runner.rs`, and `tests/core.rs`; create `tests/component_state.rs`, `tests/component_media.rs`, and `tests/interactions.rs`.

---

### Task 1: Add Gray Core’s versioned structured-input contract

**Files:**
- Create: `/home/vstaln/gray/crates/gray-core/src/input.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/lib.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/message.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/redaction.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/agent_compact.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/compact.rs`
- Modify: `/home/vstaln/gray/crates/gray-provider/src/openai.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/agent_loop.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/agent_tools.rs`
- Modify: `/home/vstaln/gray/crates/gray/src/repl/format.rs`
- Create: `/home/vstaln/gray/crates/gray-core/src/input_tests.rs`
- Create: `/home/vstaln/gray/crates/gray/src/input_json_tests.rs`
- Create/modify: `/home/vstaln/gray/crates/gray/src/print_tests.rs`

**Interfaces:**
- Produces `gray_core::input::InputEnvelope` with `protocol`, `version`, `kind`, and `payload` fields.
- Produces `InputEnvelope::from_json(bytes: &[u8]) -> Result<Self, InputError>` with a 1 MiB input cap, exact protocol/version checks, and a non-empty bounded `kind`.
- Produces `ContentBlock::StructuredInput { protocol: String, version: u32, kind: String, payload: serde_json::Value }`.
- `Message::structured_input(envelope)` constructs a user message without converting the envelope to a text block.
- The provider-facing rendering of a structured block is a bounded marker plus canonical JSON; session storage retains the typed variant.

- [ ] **Step 1: Write failing envelope and round-trip tests.**

Add tests in `input_tests.rs` for the exact contract:

```rust
#[test]
fn valid_discord_event_envelope_round_trips_as_a_typed_block() {
    let input = InputEnvelope::from_json(
        br#"{"protocol":"gray.discord.input","version":1,"kind":"component_event","payload":{"action":"refresh","values":{"id":"7"}}}"#,
    )
    .unwrap();
    assert_eq!(input.kind, "component_event");
    let block = ContentBlock::StructuredInput {
        protocol: input.protocol,
        version: input.version,
        kind: input.kind,
        payload: input.payload,
    };
    let encoded = serde_json::to_vec(&block).unwrap();
    let decoded: ContentBlock = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, block);
}

#[test]
fn envelope_rejects_unknown_protocol_version_and_non_object_payload() {
    for body in [
        br#"{"protocol":"other.input","version":1,"kind":"component_event","payload":{}}"#,
        br#"{"protocol":"gray.discord.input","version":2,"kind":"component_event","payload":{}}"#,
        br#"{"protocol":"gray.discord.input","version":1,"kind":"component_event","payload":[]}"#,
    ] {
        assert!(InputEnvelope::from_json(body).is_err());
    }
}
```

- [ ] **Step 2: Run the focused core test and verify it fails.**

Run:

```sh
cd /home/vstaln/gray
CARGO_BUILD_JOBS=4 cargo test -p gray-core input
```

Expected: compilation/test failure because `InputEnvelope` and the structured content block do not exist.

- [ ] **Step 3: Implement the core types and validation.**

Add the following shape to `input.rs` (derive/validation details must be concrete in the implementation, not left to a provider-specific caller):

```rust
pub const STRUCTURED_INPUT_PROTOCOL: &str = "gray.discord.input";
pub const STRUCTURED_INPUT_VERSION: u32 = 1;
pub const MAX_INPUT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEnvelope {
    pub protocol: String,
    pub version: u32,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum InputError {
    TooLarge,
    Json(serde_json::Error),
    UnsupportedProtocol,
    UnsupportedVersion,
    InvalidKind,
    InvalidPayload,
}
```

`from_json` must reject oversized input, non-object payloads, protocol/version mismatches, and an empty or overlong kind. The core deliberately validates the envelope shape, not Discord business rules; the plugin validates its `component_event` payload. Wire `input_tests.rs` from `gray-core/src/lib.rs` with `#[cfg(test)] #[path = "input_tests.rs"] mod input_tests;`, and wire `input_json_tests.rs` from `gray/src/lib.rs` in the same way.

- [ ] **Step 4: Add the structured content block and provider-safe rendering.**

Extend `ContentBlock` with:

```rust
StructuredInput {
    protocol: String,
    version: u32,
    kind: String,
    payload: serde_json::Value,
},
```

Add `Message::structured_input(InputEnvelope)`. Add a helper on `ContentBlock` that returns a bounded canonical provider string beginning with `<gray_structured_input kind="...">`; it must serialize the payload with sorted object keys through `serde_json::to_string`, cap the result, and never include a file body. Update `context_text` and token estimation to include the canonical representation. In both OpenAI conversion paths, emit the helper as a user text part. In assistant/tool paths, ignore a structured user block if it is encountered in an invalid role rather than panicking.

- [ ] **Step 5: Make redaction and compaction preserve the event safely.**

In `redaction.rs`, recursively walk structured payload strings with the existing path/secret redactor, preserving object keys and scalar types. In `compact.rs`/`agent_compact.rs`, charge the canonical representation, preserve the event in a user turn, and replace an oversized event with a bounded citation-style stub containing protocol, version, kind, and event ID but not values or file contents. Add a test proving a secret in `payload.values.token` is absent from the persisted redacted message.

- [ ] **Step 6: Add the CLI input group and structured print path.**

Add a non-required, single-argument Clap group to the existing `Cli` and merge these fields into that struct:

```rust
#[derive(Parser, Debug, Clone)]
#[command(group(ArgGroup::new("one_shot_input").args(["print", "input_json"])))]
pub struct Cli {
    // Keep every existing field, then add:
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "print",
        requires = "json"
    )]
    pub input_json: Option<PathBuf>,

    #[arg(long, requires = "one_shot_input")]
    pub json: bool,
}
```

Import `clap::ArgGroup`. The group is optional by default, allows at most one of `print`/`input_json`, and `json` requires the group. Keep `-p/--print` behavior unchanged. In `main.rs`, read `--input-json -` from stdin or read the named file with a 1 MiB cap, parse `InputEnvelope`, and call a new `run_print_mode_json_input` function. Factor `run_print_inner` so it accepts a `Message` rather than rebuilding the user message from a string; the existing text wrapper constructs `Message::user(prompt)`. Invalid envelopes must produce a redacted JSON error row when `--json` is set and no provider request.

- [ ] **Step 7: Run core tests and inspect persistence.**

Run:

```sh
cd /home/vstaln/gray
CARGO_BUILD_JOBS=4 cargo test -p gray-core input
CARGO_BUILD_JOBS=4 cargo test -p gray print_tests
```

Expected: valid envelopes persist as `StructuredInput`, malformed/unsupported envelopes fail before provider work, and ordinary `-p` tests remain green.

- [ ] **Step 8: Commit the core contract.**

```sh
cd /home/vstaln/gray
git add crates/gray-core/src/input.rs crates/gray-core/src/input_tests.rs \
  crates/gray-core/src/lib.rs crates/gray-core/src/message.rs \
  crates/gray-core/src/redaction.rs crates/gray-core/src/agent_compact.rs \
  crates/gray-core/src/compact.rs crates/gray-core/src/agent_loop.rs \
  crates/gray-core/src/agent_tools.rs crates/gray-provider/src/openai.rs \
  crates/gray/src/lib.rs crates/gray/src/main.rs crates/gray/src/print.rs \
  crates/gray/src/print_tests.rs crates/gray/src/repl/format.rs
git commit -m "feat(core): accept versioned structured input"
```

---

### Task 2: Define and compile the exhaustive Gray UI protocol

**Files:**
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_protocol.rs`
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_compile.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/lib.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/render.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/components.rs`

**Interfaces:**
- Produces `UiDocument`, `MessageDocument`, `ModalDocument`, `Origin`, `Surface`, and typed node enums using internally tagged `type` fields, with `#[serde(deny_unknown_fields)]` on every struct variant.
- Produces `compile_message(&MessageDocument, &CompileContext) -> Result<CompiledMessage, CompileError>` and `compile_modal(&ModalDocument, &CompileContext) -> Result<CompiledModal, CompileError>`.
- `CompileContext` owns a `ComponentIdAllocator` trait; Task 2 tests use a deterministic test allocator and Task 3 supplies the SQLite-backed allocator.
- Produces `CompiledMessage { components: Vec<serde_json::Value>, flags: u64 }` and `CompiledModal { custom_id: String, title: String, components: Vec<serde_json::Value> }`.
- `render::text_message`, `card`, `activity`, `tool_card`, and `error_card` remain trusted Rust adapters, but construct `UiDocument` values before compiling.

- [ ] **Step 1: Add failing schema fixtures for all component types.**

Create a table-driven test with one valid fixture for each type number and assert the compiler emits that number. The fixture list must contain: Action Row (1), Button (2), String Select (3), Text Input (4), User Select (5), Role Select (6), Mentionable Select (7), Channel Select (8), Section (9), Text Display (10), Thumbnail (11), Media Gallery (12), File (13), Separator (14), Container (17), Label (18), File Upload (19), Radio Group (21), Checkbox Group (22), and Checkbox (23). Add a second test with one valid modal document containing all modal controls and one valid message document containing all message controls.

- [ ] **Step 2: Run the component test and verify it fails.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test components protocol_matrix
```

Expected: failure because the typed document and compiler do not exist.

- [ ] **Step 3: Implement protocol enums with explicit fields.**

Define the public protocol as a versioned document, not a map of Discord JSON. Use the following concrete node/field contract:

```text
MessageDocument { version: u32, document_id: LogicalId, visibility: Visibility,
  lifecycle: Lifecycle, components: Vec<MessageNode> }

MessageNode = ActionRow(ActionRow) | Button(Button) | StringSelect(StringSelect) |
  UserSelect(UserSelect) | RoleSelect(RoleSelect) | MentionableSelect(MentionableSelect) |
  ChannelSelect(ChannelSelect) | Section(Section) | Text(TextDisplay) | Thumbnail(Thumbnail) |
  MediaGallery(MediaGallery) | File(FileComponent) | Separator(Separator) | Container(Container)

ModalDocument { version: u32, document_id: LogicalId, title: String,
  components: Vec<ModalNode> }

ModalNode = Label(Label) | TextInput(TextInput) | StringSelect(StringSelect) |
  UserSelect(UserSelect) | RoleSelect(RoleSelect) | MentionableSelect(MentionableSelect) |
  ChannelSelect(ChannelSelect) | FileUpload(FileUpload) | RadioGroup(RadioGroup) |
  CheckboxGroup(CheckboxGroup) | Checkbox(Checkbox) | Text(TextDisplay)
```

`Button` must carry `logical_id`, `label`, `style`, optional `url`, and optional `sku_id`; action styles require an action/field, link style requires a URL and no custom ID, and premium style is rejected without capability configuration. `ActionRow` carries 1–5 `MessageNode` children. `Section` carries 1–3 text children and one button/thumbnail accessory. `Container` allows only documented child types. `Label` has label text, optional description, and one nested modal control. Define `FileRef { id: String, name: String, media_type: String, size: u64, sha256: String }` and `MediaRef::{Remote(RemoteMedia), Attachment(FileRef)}` in this task so the compiler has no forward type dependency; Task 4 supplies the storage implementation. `File` accepts only a managed `file_id` that the media layer resolves to `attachment://`; remote media is represented by a typed `RemoteMedia` reference. Every string/value has a protocol-level maximum.

- [ ] **Step 4: Implement the compiler and validator.**

The compiler must:

- count every nested component against the 40-component message budget;
- enforce Action Row 1–5 children and one-select-or-buttons composition;
- enforce select option ranges and all documented URL/label/text/media limits;
- allocate opaque IDs through `CompileContext::allocate_id(logical_id)` and never copy an agent ID to Discord;
- set `IS_COMPONENTS_V2`, clear legacy fields, and retain no unknown protocol fields;
- reject a modal/message node on the wrong surface;
- reject style 6 unless `CompileContext::premium_enabled()` is true and a configured SKU is supplied.

Use stable errors with `code`, `path`, and `message` fields, for example `{ "code": "component_limit", "path": "$.components[0]", "message": "..." }`. Do not include the raw document or secrets in the error.

- [ ] **Step 5: Route existing trusted render helpers through the compiler.**

Change `render.rs` helpers to build typed message documents and call `compile_message`. Keep the legacy embed adapter as a narrow trusted conversion, but make all new agent/plugin code call the protocol. Delete duplicated raw type-number validation from `render.rs`; keep only the adapter bounds and compatibility tests.

- [ ] **Step 6: Add negative fixtures for every compiler rule.**

Test unknown node type, unknown field, wrong surface, duplicate logical IDs, duplicate generated IDs, too many nested components, invalid select placement, invalid option count, invalid label, invalid URL, unsupported premium SKU, missing file reference, invalid `attachment://` source, invalid spacing, invalid accent color, and modal nesting. Assert the loopback stub receives zero requests.

- [ ] **Step 7: Run focused tests and commit the protocol/compiler.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test components protocol_matrix
CARGO_BUILD_JOBS=4 cargo test --lib component_compile
git add src/component_protocol.rs src/component_compile.rs src/lib.rs src/render.rs tests/components.rs
git commit -m "feat(discord): compile exhaustive typed components"
```

---

### Task 3: Add durable documents, opaque state, and event idempotency

**Files:**
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_state.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/durable.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/lib.rs`
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/tests/component_state.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/durable.rs`

**Interfaces:**
- Produces `NewDocument`, `DocumentRecord`, `NewState`, `ResolvedState`, and `StoredComponentEvent` records.
- Produces `Store::create_document`, `Store::create_state`, `Store::resolve_state`, `Store::accept_event`, `Store::invalidate_document`, `Store::prune_component_state`, and `Store::enqueue_component_json`.
- `accept_event` takes the Discord interaction ID and performs validation, one-shot consumption, event insertion, and conversation enqueue in one SQLite transaction. The queue helper is factored as `enqueue_in_tx`; `Store::enqueue` is a wrapper around that helper so the transaction can be shared.

- [ ] **Step 1: Write failing state and transaction tests.**

Cover: document revision invalidation, wrong-user rejection, wrong-channel rejection, expiry, one-shot consumption, repeatable select state, duplicate interaction ID, concurrent duplicate clicks, and a rollback when conversation enqueue fails. Assert no raw state token appears in a stored event payload.

- [ ] **Step 2: Run the state test and verify it fails.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test component_state
```

Expected: missing state APIs/tables.

- [ ] **Step 3: Add schema migrations without breaking the existing queue.**

Add idempotent migrations in `durable.rs` for `ui_component_documents`, `ui_component_states`, `ui_component_events`, and `ui_component_files`. Also add nullable `input_json TEXT` to `inbox` and nullable `document_json TEXT` plus `document_version INTEGER` to `outbox`; old rows remain text/legacy rows. Use integer Unix seconds for the new component tables, explicit `NOT NULL` ownership fields, foreign keys, indexes on document/channel/expiry, and `UNIQUE(interaction_id)` on events. Keep the existing `component_states` cron table as a legacy compatibility table; the new physical tables use the `ui_component_` prefix to avoid colliding with it; expose a wrapper for the old cron token API until its call sites migrate.

The new state SQL must include:

```sql
CREATE TABLE IF NOT EXISTS ui_component_documents (
  document_id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  guild_id TEXT,
  channel_id TEXT NOT NULL,
  message_id TEXT,
  modal_id TEXT,
  surface TEXT NOT NULL,
  revision INTEGER NOT NULL,
  protocol_version INTEGER NOT NULL,
  status TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ui_component_states (
  token TEXT PRIMARY KEY,
  document_id TEXT NOT NULL,
  logical_id TEXT NOT NULL,
  action TEXT NOT NULL,
  user_id TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  state_json TEXT NOT NULL,
  one_shot INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  consumed_at INTEGER,
  FOREIGN KEY(document_id) REFERENCES ui_component_documents(document_id)
);
CREATE TABLE IF NOT EXISTS ui_component_events (
  interaction_id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL,
  token TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  conversation TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  delivered_at INTEGER
);
CREATE TABLE IF NOT EXISTS ui_component_files (
  file_id TEXT PRIMARY KEY,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  media_type TEXT NOT NULL,
  size INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  ref_count INTEGER NOT NULL DEFAULT 0
);
```

- [ ] **Step 4: Implement transactional state methods.**

Generate tokens with the existing cryptographic/random source, never derive them from cron IDs or logical names. Resolve state by opaque token and compare owner/channel/document/revision/expiry. `accept_event` inserts the normalized event and calls `enqueue_in_tx` with the canonical envelope inside one transaction; a duplicate interaction ID returns the original receipt and does not enqueue again. Revoke all states for older revisions when a document is edited or cancelled. `enqueue_component_json` is the public non-transactional wrapper used by tests and administrative tools.

- [ ] **Step 5: Add pruning and restart tests.**

Prune terminal events/documents/states after retention, but never delete a live file referenced by a state. Reopen the SQLite file and assert documents, opaque mappings, idempotency keys, and queued component inputs survive. Keep the existing durable queue tests green.

- [ ] **Step 6: Run and commit state work.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test component_state --test durable
git add src/component_state.rs src/durable.rs src/lib.rs tests/component_state.rs tests/durable.rs
git commit -m "feat(discord): persist component state and events"
```

---

### Task 4: Add managed media and multipart file descriptors

**Files:**
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_media.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/durable.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/lib.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/Cargo.toml`
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/tests/component_media.rs`

**Interfaces:**
- Produces `FileStore::import_bytes`, `import_url`, `import_generated`, `open_for_conversation`, `metadata`, and `multipart_part`.
- Produces `FileRef { id, name, media_type, size, sha256 }` and `MediaRef::{Remote(RemoteMedia), Attachment(FileRef)}`.
- The compiler consumes `FileRef`/`MediaRef`; it never receives a host path.

- [ ] **Step 1: Write failing media tests.**

Test import bytes, duplicate hash reuse, private file modes, byte/count limits, media type rejection, remote HTTP(S) URL acceptance, private-address rejection, generated-file import restricted to the conversation workdir, symlink escape rejection, safe conversation path generation, expiry, and cleanup after the last reference is released. Add a multipart test that asserts the request contains `payload_json`, one file part, a sanitized filename, and no token in the URL/body.

- [ ] **Step 2: Run the media test and verify it fails.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test component_media
```

Expected: missing `component_media` APIs and multipart feature.

- [ ] **Step 3: Add the bounded file store.**

Enable `reqwest`’s `multipart` feature. Store files under a `0700` media directory beside the conversation home, use `0600` files, compute SHA-256, sanitize names, and cap individual/file-total bytes. Validate remote URLs with `reqwest::Url`, allow only `http`/`https`, reject loopback/private/link-local hosts, and cap download size. Never follow a redirect to a disallowed host.

- [ ] **Step 4: Implement stable file references and cleanup.**

`FileRef.id` is an opaque random ID; `FileRef.name` is safe for Discord attachment naming. `import_generated` accepts only a canonical path under the current conversation workdir, rejects symlink escapes, copies the bytes into the managed store, and returns a `FileRef`. `open_for_conversation` returns a path under the conversation work directory only after checking ownership. `multipart_part` reads the managed file and returns a reqwest `Part`; it does not log bytes. Prune expired unreferenced files and retain referenced files until state/document expiry.

- [ ] **Step 5: Run and commit media work.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test component_media
CARGO_BUILD_JOBS=4 cargo check
 git add Cargo.toml Cargo.lock src/component_media.rs src/durable.rs src/lib.rs tests/component_media.rs
git commit -m "feat(discord): manage component media files"
```

---

### Task 5: Extend the REST transport for every V2 response and upload

**Files:**
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/transport.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_compile.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/delivery.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/components.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/common/mod.rs`

**Interfaces:**
- Preserve `Rest::send_v2`, `edit_message_v2`, `interaction_v2`, `defer_v2`, `edit_original_v2`, and `interaction_update_v2` for existing trusted callers.
- Add `Rest::send_compiled(channel, &CompiledMessage, &[FileRef]) -> Result<MessageId, TransportError>`.
- Add `Rest::open_modal`, `Rest::callback`, `Rest::defer_update`, `Rest::update_message`, `Rest::modal_response`, and `Rest::autocomplete_response` methods using interaction response types 4, 5, 6, 7, 8, and 9.
- Add `Rest::edit_compiled` and `Rest::edit_original_compiled`; all methods accept only compiled output.

- [ ] **Step 1: Add failing loopback tests for routes and multipart.**

Assert exact paths and bodies for callback type 4, deferred type 5, deferred modal update type 6, message update type 7, autocomplete type 8, modal response type 9, original webhook edit, V2 message create/edit, and multipart attachment upload. Assert every message-bearing body has `allowed_mentions.parse == []` and V2 flags where the response supports flags, every V2 message has no legacy content/embeds, and no token appears in any log/error.

- [ ] **Step 2: Run the delivery tests and verify they fail.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test delivery v2_
```

Expected: missing new methods/routes.

- [ ] **Step 3: Implement response builders.**

Use the Discord interaction routes:

```text
POST /interactions/{interaction_id}/{token}/callback       type 4
POST /interactions/{interaction_id}/{token}/callback       type 5
POST /interactions/{interaction_id}/{token}/callback       type 6
POST /interactions/{interaction_id}/{token}/callback       type 7
POST /interactions/{interaction_id}/{token}/callback       type 8
POST /interactions/{interaction_id}/{token}/callback       type 9
PATCH /webhooks/{application_id}/{token}/messages/@original
POST /webhooks/{application_id}/{token}
```

The transport must set `Content-Type: application/json` for ordinary calls and `multipart/form-data` for files, use the existing safe User-Agent, update route/global buckets from response headers, honor 429 `retry_after`, and classify 5xx/network failures as retryable. Interaction endpoints do not consume the global bucket.

- [ ] **Step 4: Implement multipart `payload_json` exactly once.**

Build the JSON payload from `CompiledMessage` and attach each managed file with a sanitized unique filename. For V2 File/Thumbnail/Media Gallery references, replace `FileRef` placeholders with `attachment://filename` only after the upload plan is known. Reject any unresolved file reference before opening the request.

- [ ] **Step 5: Run and commit transport work.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test delivery --test components
CARGO_BUILD_JOBS=4 cargo clippy --all-targets -- -D warnings
git add src/transport.rs src/component_compile.rs tests/delivery.rs tests/components.rs tests/common/mod.rs
git commit -m "feat(discord): transport full components v2 interactions"
```

---

### Task 6: Normalize Gateway interactions and route typed events

**Files:**
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_input.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/gateway.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/command_dispatch.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/durable.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/lib.rs`
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/tests/interactions.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/components.rs`

**Interfaces:**
- Produces `NormalizedInteraction::{Button, Select, ModalSubmit, Autocomplete}` with `interaction_id`, `user_id`, `channel_id`, `guild_id`, `message_id`, `modal_id`, `custom_id`, logical component ID, values, and file IDs.
- Produces `NormalizedInteraction::gray_envelope(conversation: &str) -> serde_json::Value` matching core `gray.discord.input` version 1.
- Produces `Gateway::handle_component_interaction(...)`; legacy `command_dispatch::button` remains only for old rows and old `cron:` tokens.

- [ ] **Step 1: Write failing normalization fixtures.**

Create JSON fixtures for all four Discord interaction data forms: message component button, each select kind, modal submit with text/select/checkbox/radio/upload fields, and autocomplete. Assert logical IDs and typed values are extracted, Discord opaque IDs remain internal, and an unknown component type fails closed.

- [ ] **Step 2: Run the interaction test and verify it fails.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test interactions
```

Expected: missing normalization and event routing APIs.

- [ ] **Step 3: Implement the normalized input types and envelope.**

Parse the Twilight interaction into a bounded `serde_json::Value` representation without exposing Twilight types beyond `gateway.rs`. For modal file-upload fields, copy each referenced Discord attachment through `FileStore::import_bytes` before building the event; reject missing, oversized, or unauthorized attachments. Require a state token, resolve it through `Store`, and construct the core envelope with `protocol = "gray.discord.input"`, `version = 1`, `kind = "component_event"`, and a payload containing conversation, document, logical component/action, typed values, and managed file IDs. Do not place the Discord token, raw Discord body, or custom ID in the envelope.

- [ ] **Step 4: Implement atomic acknowledgement and response selection.**

Call `Store::accept_event` before responding. For a button/select, send type 4 with ephemeral flags when the document says ephemeral, or type 7 when the document requires updating the original. For modal validation/errors, send type 9; for successful modal acknowledgement, use type 4 or the document’s channel/message policy. For autocomplete, validate the focused state and send a bounded type 8 option list. A failed state/permission check sends a bounded ephemeral error and does not enqueue. All paths must finish within the three-second callback budget; slow Gray work happens after acknowledgement.

- [ ] **Step 5: Replace raw custom-ID routing while preserving migration behavior.**

In the `InteractionCreate` branch, route `MessageComponent`, `ModalSubmit`, and `Autocomplete` through the new normalizer first. Only a legacy token with the existing `cron:` prefix may call the old command handler. Slash commands continue through `command_dispatch::handle`. Ensure no `custom_id` string is logged.

- [ ] **Step 6: Test queue ordering and idempotency.**

Use the loopback stub and a temporary SQLite store. Submit two events for one conversation in order, restart the runtime between them, and assert the same session receives both typed envelopes. Submit the same Discord interaction twice and assert one inbox row and one callback. Submit an event from another user/channel and assert no inbox row and no side effect.

- [ ] **Step 7: Run and commit Gateway integration.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test interactions --test components --test durable
git add src/component_input.rs src/gateway.rs src/command_dispatch.rs src/durable.rs src/lib.rs tests/interactions.rs tests/components.rs
git commit -m "feat(discord): route component events into gray"
```

---

### Task 7: Add native structured turns to the isolated runner

**Files:**
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/durable.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/runner.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/gateway.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/session.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/runner.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/runtime.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/core.rs`

**Interfaces:**
- Produces `RunInput::{Text(String), Structured(serde_json::Value)}` and `Store::enqueue_component_event`.
- `run_gray` remains the ordinary text wrapper; add `run_gray_input` for the enum.
- `InboxItem` exposes `input: RunInput` while retaining `prompt` for legacy text rows.

- [ ] **Step 1: Write failing runner tests.**

Use the existing fixture shell binary to record argv and stdin. Assert ordinary prompts still contain `-p`, structured events invoke `--input-json` with a `0600` temporary file, no event JSON is placed on the command line, the same `--session` is used on continuation, and the file is removed after the child exits. Assert a malformed event is rejected before spawning Gray.

- [ ] **Step 2: Run the runner test and verify it fails.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test runner structured_input
```

Expected: missing `RunInput`/`--input-json` path.

- [ ] **Step 3: Extend the durable inbox schema.**

Use the `input_json TEXT` migration added in Task 3: backfill null values as text input, make `claim` deserialize structured rows into `RunInput::Structured`, and add `enqueue_component_event` as the typed wrapper around `Store::enqueue_component_json`. Keep `enqueue` unchanged for ordinary text and cron rows.

- [ ] **Step 4: Add the structured runner invocation.**

Add:

```rust
pub enum RunInput {
    Text(String),
    Structured(serde_json::Value),
}
```

`run_gray_input` validates the envelope, writes a canonical JSON file under the conversation home with mode `0600`, and builds args `[gray_bin, "--input-json", path, "--json", "--max-requests", N]`. For `RunInput::Text`, retain the existing `-p prompt` args. Keep the existing environment scrubbing, session lock, budget reservation, timeout, NDJSON parser, and redaction. Pass file IDs as metadata only; the model obtains readable files through the safe plugin file operation.

- [ ] **Step 5: Change Runtime’s runner callback without breaking text callers.**

Change the callback input to `&RunInput`. When a claimed inbox item is text, construct `RunInput::Text`; when it is structured, pass the JSON. Update the Gateway closure and all test fixtures. The activity/progress callback remains unchanged and receives only Gray NDJSON rows.

- [ ] **Step 6: Test same-session continuation and restart.**

Create a temporary conversation with one text turn and one component event. Assert both result rows have the same Gray session ID, the event is persisted as structured input, and closing/reopening the queue does not change the session pointer. Assert a component event never becomes the literal prompt string in the transcript.

- [ ] **Step 7: Run and commit runner integration.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test runner --test runtime --test core
git add src/durable.rs src/runner.rs src/gateway.rs src/session.rs tests/runner.rs tests/runtime.rs tests/core.rs
git commit -m "feat(discord): run gray with structured component input"
```

---

### Task 8: Expose typed document operations through the sidecar

**Files:**
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/sidecar.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/render.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_compile.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/component_media.rs`
- Create: `/home/vstaln/grayplugins/gray-discord-plugin/tests/sidecar.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/capabilities.rs`

**Interfaces:**
- Extends `discord_send` with `{ "operation": "send", "document": { ... } }`, `edit`, `open_modal`, `update`, and `disable` operations while preserving `{ "content": "..." }`.
- Adds `discord_file` with `{ "file_id": "...", "action": "path" | "metadata" }` and a generated-file `import` action that accepts only a canonical path under the current conversation workdir.
- Sidecar returns `{ ok, document_id, message_id, modal_id, revision, component_count, expires_at, warnings }` without secrets.

- [ ] **Step 1: Write failing sidecar schema/dispatch tests.**

Test manifest schema shape, content-only compatibility, agent document compilation, operation authorization, unknown fields, missing file IDs, and the fact that an agent cannot set `origin: plugin` or a Discord `custom_id`. Use a loopback REST stub and temporary store; no live Discord access.

- [ ] **Step 2: Run the sidecar test and verify it fails.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test sidecar
```

Expected: the current manifest/dispatch accepts only `content` and returns no document receipt.

- [ ] **Step 3: Extend the manifest without exposing raw Discord schema.**

Describe `document` as a Gray protocol object with a `version`, `surface`, and typed node variants. Add a tool description stating that all interactive inputs become structured events in the same conversation, and that files must use `discord_file`/managed IDs. The model-facing schema must not describe Discord numeric types, `custom_id`, callback URLs, or arbitrary host paths.

- [ ] **Step 4: Implement sidecar operation dispatch.**

For `content`, compile a trusted `TextDisplay` document. For `document`, deserialize with `deny_unknown_fields`, force `Origin::Agent`, allocate state, compile, and send/edit through the transport. `open_modal` requires an opaque interaction handle supplied by a normalized component event; resolve it in the store and never accept a raw Discord token from tool arguments. `update` and `disable` require a document ID plus current revision and owner binding. `discord_file` resolves a managed file, imports a generated file only from the current conversation workdir, and returns only a safe conversation-workdir path or metadata.

- [ ] **Step 5: Make all errors structured and secret-safe.**

Return stable `code` values such as `invalid_document`, `unknown_operation`, `not_interaction_origin`, `state_conflict`, `file_not_found`, and `delivery_failed`. Include only bounded field paths and safe names. Keep the existing `is_error` shape and frame-size limit.

- [ ] **Step 6: Run and commit sidecar work.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test sidecar --test capabilities --test components
git add src/sidecar.rs src/render.rs src/component_compile.rs src/component_media.rs tests/sidecar.rs tests/capabilities.rs
git commit -m "feat(discord): expose typed component tool operations"
```

---

### Task 9: Migrate every plugin output path to compiled documents

**Files:**
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/activity.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/commands.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/command_dispatch.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/cron.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/pairing.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/src/transport.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/activity.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/cron_delivery.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/pairing.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/components.rs`

**Interfaces:**
- All new outgoing messages use `compile_message` and `Rest::send_compiled`/`edit_compiled`.
- `Store::complete` may persist a typed document/receipt alongside legacy text for activity/tool cards and cron output.
- Existing `OutboxPart::render` values remain readable, but new rows set `render = "v2"` and populate the Task 3 `document_json`/`document_version` fields with a serialized Gray protocol document.

- [ ] **Step 1: Write failing migration assertions.**

For activity start/update, tool card, pairing, slash command response, cron result, and error paths, assert the loopback request has `flags == 32768`, no `content`, no `embeds`, and a valid compiled component tree. Keep a fixture for one old queue row and assert it remains readable through the compatibility transport.

- [ ] **Step 2: Run migration tests and verify they fail.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test activity --test cron_delivery --test pairing
```

Expected: current paths still use legacy `send`/embed helpers or do not retain typed document metadata.

- [ ] **Step 3: Convert activity and command renderers.**

Replace `render::activity`, `tool_card`, `error_card`, `from_embed`, and command button JSON with typed documents. Buttons receive logical IDs and state tokens through the compiler; command handlers never construct Discord numeric JSON. Pairing uses a short-lived stateful document with a typed action event, while the existing pairing approval command remains a plugin command.

- [ ] **Step 4: Convert delivery and cron output.**

When the Gateway’s delivery closure sees `render = "v2"`, deserialize/compile the stored typed document and deliver the compiled tree. Preserve nonce and retry behavior. Cron replies use the same document compiler and text-to-TextDisplay adapter. Add a bounded document revision so an edit cannot silently change an already acknowledged interaction.

- [ ] **Step 5: Run and commit output migration.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo test --test activity --test cron_delivery --test pairing --test delivery
git add src/activity.rs src/commands.rs src/command_dispatch.rs src/cron.rs src/pairing.rs src/transport.rs tests/activity.rs tests/cron_delivery.rs tests/pairing.rs tests/components.rs
git commit -m "refactor(discord): route all output through components v2"
```

---

### Task 10: Complete the end-to-end verification matrix and documentation

**Files:**
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/README.md`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/docs/port/CONFORMANCE.md`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/docs/RUNTIME.md`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/components.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/interactions.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/component_media.rs`
- Modify: `/home/vstaln/grayplugins/gray-discord-plugin/tests/runner.rs`
- Modify: `/home/vstaln/gray/crates/gray-core/src/input_tests.rs`
- Modify: `/home/vstaln/gray/crates/gray/src/input_json_tests.rs`

**Interfaces:**
- Documentation describes the versioned Gray UI protocol, native `--input-json` mode, every supported component/input type, media/file behavior, trust boundaries, and legacy fallback.
- The complete test suite has a single loopback fixture and no live Discord/provider/network dependency.

- [ ] **Step 1: Add a table-driven end-to-end matrix.**

Add one test row for each component type and each interaction response path:

```text
message: 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 17
modal: 3, 4, 5, 6, 7, 8, 10, 18, 19, 21, 22, 23
interactions: button, each select, modal submit, autocomplete, callback 4,
defer 5, defer-update 6, update 7, autocomplete 8, modal 9
```

Each row must assert the exact Discord type, V2 flags where applicable, mention policy, state token opacity, and event envelope values.

- [ ] **Step 2: Add restart, failure, and compatibility scenarios.**

Cover: service restart during event enqueue, queue full, SQLite busy/rollback, expired state, old queue row, old core without `--input-json`, invalid component tree before HTTP, 429 retry, 5xx retry, 3-second acknowledgement, upload failure with no partial V2 message, file expiry, and redaction of secret-shaped event values. Assert failures never duplicate events or leak tokens/file contents.

- [ ] **Step 3: Update operator documentation.**

Document the exact commands and schemas:

```sh
gray --input-json /path/to/event.json --json --session <id>
gray --input-json - --json --session <id> < event.json
```

Show a typed message document, a typed modal document, a `discord_send` document operation, and a `discord_file` operation. State that Discord numeric types/custom IDs are plugin-owned, events join the same Gray session, and legacy text calls remain supported. Update the conformance table with every component type and every interaction response type; do not claim voice or arbitrary premium billing support.

- [ ] **Step 4: Run the complete verification suite.**

```sh
cd /home/vstaln/gray
CARGO_BUILD_JOBS=4 cargo fmt --check
CARGO_BUILD_JOBS=4 cargo test -p gray-core
CARGO_BUILD_JOBS=4 cargo test -p gray
CARGO_BUILD_JOBS=4 cargo clippy --workspace -- -D warnings

cd /home/vstaln/grayplugins/gray-discord-plugin
CARGO_BUILD_JOBS=4 cargo fmt --check
CARGO_BUILD_JOBS=4 cargo clippy --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 cargo test --all-targets
```

Expected: all commands pass. The project’s CI gate is fmt, workspace clippy, and workspace tests; the plugin additionally runs all targets because it has fixture-based transport tests.

- [ ] **Step 5: Inspect the final diff and commit documentation/tests.**

```sh
cd /home/vstaln/grayplugins/gray-discord-plugin
git diff --check
git status --short
git add README.md docs/port/CONFORMANCE.md docs/RUNTIME.md \
  tests/components.rs tests/interactions.rs tests/component_media.rs tests/runner.rs
git commit -m "docs(discord): document typed components v2"
```

Do not push, merge, or open another PR. Report the existing PR #2 state and ask before any integration action.

## Plan Self-Review

- **Spec coverage:** The plan covers all 20 component numbers, every listed modal/message surface, all six new interaction response paths plus callback/defer, native core input, trust levels, state/idempotency, media/files, transport/rate limits, migration, failure handling, documentation, and the full verification matrix.
- **Placeholder scan:** The plan contains no `TODO`, `TBD`, or instruction to leave an implementation unspecified. Each task names exact files, interfaces, tests, commands, and commit paths.
- **Type consistency:** Core uses `InputEnvelope`/`ContentBlock::StructuredInput`; the plugin creates that exact wire shape; the runner uses `RunInput`; the state layer returns opaque tokens; the compiler is the only component-to-Discord boundary; the transport accepts only compiled output; Gateway creates the normalized event; sidecar and activity/cron use the same compiler. `FileRef` is defined in the protocol task and implemented by the media task; `RunInput` consumes the queue schema added in the state task.
- **Dependency order:** Core input contract precedes runner invocation; protocol/compiler precedes state and transport; state/media precede Gateway; Gateway and runner precede sidecar migration; final tests span both repositories.
- **Scope check:** The work is architectural and cross-repository, but the milestones are dependency-ordered rather than independent. Keeping one plan avoids two incompatible partial UI protocols. Execution should still stop at each task’s test/commit gate for review.
