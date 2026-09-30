# Gray Discord: Exhaustive Components V2 and Typed Interaction Design

**Date:** 2026-09-24  
**Status:** Design approved; implementation plan pending  
**Scope:** `gray-discord-plugin` plus a small structured-input contract in Gray core

## 1. Problem

Gray Discord currently has a text-first Discord surface. It can send text, render
legacy command embeds, and expose a small number of buttons. That is insufficient
when the agent needs to present rich information, collect structured input, open
forms, upload files, or route an interaction back into the same conversation.

The new surface must make Components V2 a first-class protocol rather than a
one-way formatting trick. The plugin owns Discord correctness and lifecycle;
Gray owns the semantic intent; the agent may author a broad, typed UI surface
without being trusted with raw Discord JSON or privileged actions.

## 2. Goals

- Cover every currently documented Discord message and modal component type.
- Support arbitrary valid message layouts, not only text cards.
- Support buttons, all select menus, text inputs, file upload, radio and
  checkbox controls, and modal submission.
- Preserve typed interaction input when it enters Gray; do not flatten it into
  an untyped prompt string.
- Deliver interaction events into the same conversation/session as ordinary
  input, with ordering, deduplication, expiry, and authorization.
- Support remote media, galleries, thumbnails, generated files, and uploaded
  files through managed references and Discord multipart APIs.
- Keep Discord-specific validation, rate limits, permissions, retries, and
  error redaction inside the plugin.
- Preserve the existing text API and existing Gray CLI behavior.

## 3. Non-goals and explicit boundaries

- The agent does not send raw Discord component JSON.
- The agent does not choose Discord `custom_id` values.
- Agent-authored components cannot invoke privileged plugin operations merely
  by naming an action. Privileged actions are registered in Rust and checked
  separately.
- Gray core does not contain Discord transport or component translation code.
- The plugin does not become a second model runtime; it remains the Discord
  gateway, queue, protocol, and delivery boundary.
- Voice channels and non-component Discord interactions remain outside this
  design.
- Premium button style is represented in the schema but fails closed unless a
  separately configured SKU/monetization capability is enabled. This avoids
  silently treating a premium button as a normal action.

## 4. Component inventory

The protocol has one versioned document model with message and modal surfaces.
The compiler emits Discord's numeric component types only after validation.

### 4.1 Message components

| Gray node | Discord type | Support and constraints |
|---|---:|---|
| `action_row` | 1 | Top-level or nested in a container; 1–5 children; buttons or one select |
| `button` | 2 | Action styles 1–4 with a logical action; link style 5 with a validated URL; premium style 6 capability-gated |
| `string_select` | 3 | 1–25 options; logical field and bounded values |
| `user_select` | 5 | Discord-populated user values; logical field |
| `role_select` | 6 | Discord-populated role values; logical field |
| `mentionable_select` | 7 | Discord-populated user/role values; logical field |
| `channel_select` | 8 | Channel filters and logical field |
| `section` | 9 | 1–3 Text Display children and a Button or Thumbnail accessory |
| `text` | 10 | Markdown Text Display; allowed-mentions policy is explicit |
| `thumbnail` | 11 | Section accessory; media object with URL or attachment reference |
| `media_gallery` | 12 | 1–10 media items with descriptions/spoiler flags |
| `file` | 13 | One `attachment://` file reference per component |
| `separator` | 14 | Optional divider and spacing 1 or 2 |
| `container` | 17 | Accent color, spoiler flag, and documented child components |

### 4.2 Modal components

| Gray node | Discord type | Support and constraints |
|---|---:|---|
| `text_input` | 4 | Short or paragraph input, required/default/min/max/placeholder validation |
| `label` | 18 | Label text, optional description, and one nested modal control |
| `file_upload` | 19 | Managed upload input with count, min/max, and accepted file constraints |
| `radio_group` | 21 | One selected option, bounded options, required state |
| `checkbox_group` | 22 | Multiple selected values with min/max validation |
| `checkbox` | 23 | Boolean checked state and label |
| `string_select` | 3 | Modal select with options and min/max values |
| `user_select` | 5 | Modal user select |
| `role_select` | 6 | Modal role select |
| `mentionable_select` | 7 | Modal mentionable select |
| `channel_select` | 8 | Modal channel select |
| `text` | 10 | Text Display used as modal description/content where supported |

The protocol covers all 20 component type numbers currently listed by Discord's
reference. A future Discord component type is rejected until the Gray enum,
compiler, validator, event parser, and tests are updated together.

## 5. Typed Gray component protocol

The sidecar accepts a Gray-owned document, not Discord JSON. The document has a
version and a surface:

```json
{
  "protocol": "gray.discord.ui",
  "version": 1,
  "surface": "message",
  "document_id": "status-42",
  "origin": "agent",
  "visibility": "public",
  "components": [
    {
      "type": "container",
      "accent": "blue",
      "children": [
        {"type": "text", "markdown": "Choose an action"},
        {
          "type": "action_row",
          "children": [
            {
              "type": "button",
              "action": "refresh",
              "label": "Refresh",
              "style": "primary"
            }
          ]
        }
      ]
    }
  ]
}
```

`origin` is assigned by the host boundary and is not trusted when supplied by an
agent. The wire representation is deserialized into Rust enums before any
Discord request. Unknown fields, unknown node types, invalid combinations, and
unbounded values fail validation.

### 5.1 Logical identity versus Discord identity

Agent/plugin nodes use logical IDs such as `refresh` or `query`. The plugin:

1. validates the logical IDs and event schema;
2. creates a durable document/state record;
3. allocates an opaque random Discord `custom_id`;
4. stores the mapping from opaque ID to logical ID, document, version, owner,
   channel, and expiry;
5. emits the Discord component tree.

The logical ID is returned in the typed interaction event. The Discord opaque ID
is never exposed to the agent or placed in model-visible state.

### 5.2 Document operations

The typed sidecar/tool supports:

- `send_message(document)`;
- `edit_message(document_id, revision, document)`;
- `open_modal(document)`;
- `update_message(document_id, revision, document)`;
- `disable_component(document_id, component_id)`;
- `cancel_document(document_id)`;
- `attach_file(file_id)` and `attach_media(media_reference)`.

Every operation returns a structured receipt:

```json
{
  "ok": true,
  "document_id": "status-42",
  "message_id": "1234567890",
  "surface": "message",
  "component_count": 2,
  "revision": 1,
  "expires_at": 1780000000,
  "warnings": []
}
```

The existing `{ "content": "..." }` form remains valid. It compiles to a
one-node V2 Text Display and uses the same queue, mentions, retry, and error
paths.

## 6. Trust model

### Plugin-authored components

Plugin code may reference a registered action schema. Registration includes:

- action name and version;
- required permission/capability;
- input schema;
- whether it is single-use or repeatable;
- allowed event kinds and maximum input size.

A user interaction with a plugin-authored component is still normalized and
delivered to Gray as a typed event. The plugin may perform only protocol-level
acknowledgement or UI updates; it does not execute the action as a hidden side
effect. This keeps one event path and one policy/audit trail.

### Agent-authored components

The agent may compose the complete visual and input surface, including modal
controls and file upload fields. Its interactions produce typed events only.
The event can request an action, but the action is executed by Gray after the
event enters the conversation, subject to the normal agent/tool policy.

The agent cannot:

- set a Discord `custom_id`;
- choose an arbitrary callback URL or webhook;
- access another user's/guild's state;
- bypass channel permissions or allowlists;
- read arbitrary managed files;
- make the plugin execute an unregistered privileged operation.

## 7. Native Gray structured input

Gray core gains a new one-shot input mode without changing normal text mode.
The preferred invocation is a versioned JSON file or stdin stream:

```sh
gray --input-json /path/to/event.json --json --session <id>
gray --input-json - --json --session <id> < event.json
```

`--input-json` is mutually exclusive with the existing `-p/--print` prompt. The
input envelope is validated by core before provider work:

```json
{
  "protocol": "gray.discord.input",
  "version": 1,
  "kind": "component_event",
  "conversation": {
    "platform": "discord",
    "conversation_id": "discord:channel:42:user:123",
    "channel_id": "42"
  },
  "event": {
    "event_id": "discord-interaction-id",
    "kind": "modal_submit",
    "document_id": "settings-42",
    "component_id": "query",
    "action": "search",
    "values": {
      "query": "needle",
      "include_files": true
    },
    "files": []
  }
}
```

The core contract preserves the event object for the model/session layer and
persists it in the same session stream. It is not serialized into a prose
prompt. Existing `gray -p "..."` and normal REPL input remain unchanged.

The Discord plugin stores the event durably before acknowledging the Discord
interaction. The runtime passes the envelope to the same Gray session in order.
If a turn is already running, the event is the next ordered input for that
conversation. It does not mutate or cancel the running child process; ordering
and session continuity are preserved by the durable queue.

## 8. Interaction state and event lifecycle

SQLite owns the following logical records. Because the plugin already has a
legacy `component_states` table for cron buttons, the new physical tables use
`ui_component_` prefixes:

- `ui_component_documents`: logical ID, revision, surface, owner, channel, guild,
  message/modal ID, protocol version, expiry, and status;
- `ui_component_states`: opaque token, logical component/action, document,
  user/channel binding, input schema, one-shot/repeatable policy, expiry, and
  consumed state;
- `ui_component_events`: Discord interaction ID, event kind, normalized values,
  file IDs, conversation, delivery status, and timestamps;
- `ui_component_files`: managed path, media type, size, hash, owner, expiry, and
  Discord attachment reference.

### 8.1 Button

1. Receive `MESSAGE_COMPONENT` interaction.
2. Resolve the opaque state and validate user, channel, guild, message,
   document revision, expiry, and action schema.
3. Atomically record the event using the Discord interaction ID as an idempotency
   key.
4. Enqueue it into the originating conversation.
5. Acknowledge with callback type 4, update the original with type 7, or show a
   bounded ephemeral response according to the document policy.

### 8.2 Selects

The normalized event contains the selected option values and/or Discord IDs.
Range, option, duplicate, and maximum-value validation happens before enqueue.
Select events may be one-shot or repeatable according to document policy.

### 8.3 Modal submission

The normalized event contains all text fields, selected values, booleans, and
managed upload IDs. Submission is atomic: either the complete event is recorded
or no state is consumed. The plugin responds with modal interaction type 9 when
it needs to show a validation result; otherwise it acknowledges with the
appropriate channel/message response.

### 8.4 Autocomplete

Autocomplete is a short-lived request path. The plugin validates focused state
and returns a bounded option list using interaction response type 8. The
resulting selection becomes a normal field value in the later modal event.

### 8.5 Expiry, replay, and revision changes

Expired or mismatched state is deleted or marked terminal and never reaches
Gray. Revoking or editing a document invalidates all older state versions.
Discord interaction IDs are unique in the event table, so retries cannot create
duplicate conversation inputs.

## 9. Media and file lifecycle

The plugin supports:

- remote image/video URLs;
- media galleries and thumbnails;
- generated files produced by Gray tools;
- files uploaded through Discord message attachments;
- files uploaded through modal file-upload controls;
- `attachment://` references in V2 File/Thumbnail/Media Gallery components.

Files are copied into a private managed store before being exposed to Gray. Each
file has a stable ID, media type, size, hash, owner, expiry, and redacted
display name. Generated files may be imported only from a canonical path under
the current conversation work directory; symlink escapes and arbitrary host
paths are rejected. The plugin enforces byte limits, count limits, allowed media
types, path safety, and permission checks.

Discord uploads use multipart requests with the correct `payload_json` component
metadata. The plugin never accepts an arbitrary host path from a Discord event
and never logs file contents. Expired files are pruned; referenced files cannot
be deleted until all live documents/events release them.

## 10. Transport responsibilities

The transport layer owns all Discord wire concerns:

- V2 create/edit with `IS_COMPONENTS_V2` and cleared legacy fields;
- callback, defer, update, modal, and autocomplete responses;
- original interaction response edits and followups;
- message/attachment uploads and multipart boundaries;
- channel and thread permissions;
- Discord route/global rate-limit buckets and 429 parsing;
- three-second interaction acknowledgement budgets;
- safe retryable/non-retryable error classification.

The transport never accepts an unvalidated component tree. The compiler runs
before the request and returns structured validation errors.

## 11. Security and quotas

The following are enforced independently:

- Discord's 40-component message budget and modal-specific limits;
- nesting, label, URL, option, media, and file constraints;
- per-document, per-conversation, per-user, and global event quotas;
- allowlist and guild/channel authorization;
- state TTL and maximum state size;
- upload byte/count/type limits;
- mention policy (`allowed_mentions` is explicit and safe by default);
- rate limits and interaction acknowledgement deadlines;
- trusted versus agent-authored action policy.

Errors expose stable categories and correlation IDs only. Tokens, raw Discord
bodies, raw file contents, and provider responses never cross the user/log
boundary.

## 12. Compatibility and migration

- Existing text calls compile to V2 Text Display.
- Existing `discord_send` parameters remain valid.
- Existing command/cron/activity outputs migrate to the typed document compiler.
- Legacy queue rows are marked and delivered through the old transport only
  until they drain; new rows are V2.
- Core advertises structured-input protocol version 1. Older cores receive a
  clear compatibility error rather than silently flattening events.
- The plugin advertises message/modal/media capabilities to Gray.
- Existing Discord intents remain unchanged; Components V2 is a message format,
  not a replacement gateway intent.

## 13. Module boundaries

The implementation is expected to introduce these focused modules:

- `component_protocol.rs`: Gray enums, serialization, logical IDs, versions;
- `component_compile.rs`: protocol-to-Discord validation and compilation;
- `component_state.rs`: durable documents, state, events, and idempotency;
- `component_media.rs`: managed files and multipart upload descriptors;
- `component_input.rs`: normalized interaction event envelope;
- `transport.rs`: V2/message/modal/media wire operations;
- `gateway.rs`: Gateway event normalization and routing;
- `runner`/sidecar boundary: structured output and native input invocation.

Each module has unit tests; integration tests use the loopback Discord fixture and
must never contact live Discord or use real credentials.

## 14. Verification matrix

The implementation is complete only when all of the following pass:

1. Every listed message and modal component compiles to a valid documented shape.
2. Every supported interaction response type is exercised against the fixture.
3. Unknown component types, invalid nesting, duplicate IDs, over-budget trees,
   invalid URLs, invalid media, and invalid modal values fail before HTTP.
4. Agent and plugin trust levels are both tested, including privilege rejection.
5. State ownership, channel binding, expiry, revision invalidation, replay,
   concurrent clicks, and Discord interaction idempotency are tested.
6. Text, select, modal, upload, and autocomplete events reach the same Gray
   session in order and survive restart.
7. Multipart upload, attachment retention, gallery display, and managed-file
   expiry are tested.
8. Original-response edits, followups, type 7 updates, type 8 autocomplete, and
   type 9 modal responses are tested.
9. Rate-limit headers, 429 bodies, retry backoff, and interaction deadlines are
   tested.
10. Existing plugin tests and Gray core structured-input tests remain green.

## 15. Acceptance criteria

The feature is accepted when an allowlisted Discord user can receive a rich V2
message containing any supported message component, interact with every supported
message control, complete every supported modal control including file upload,
and have the resulting typed event resume the same Gray conversation. The agent
can author the visual surface through the typed protocol, while the plugin
retains exclusive control of Discord IDs, state, permissions, uploads, retries,
and wire validation. Text-only callers continue to work unchanged.

No implementation begins until this spec is reviewed and the implementation plan
is written.
