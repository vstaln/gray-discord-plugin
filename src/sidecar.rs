//! Port of gray_discord/sidecar.py: NDJSON protocol 1.1 sidecar.
//! Manifest answers without config; delivery failures never echo secrets.
use serde_json::{json, Value};
use std::path::Path;

use crate::component_compile::{compile_message, compile_modal, CompileContext};
use crate::component_media::FileStore;
use crate::component_protocol::{DocumentRequest, FileRef, MessageNode, Origin, UiDocument};
use crate::component_state::StoreAllocator;
use crate::durable::Store;
use std::sync::LazyLock;

/// Static manifest (`discord` 0.1.0, protocol 1.1, no commands).
pub static MANIFEST: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "name": "discord",
        "version": "0.1.0",
        "protocol": "1.1",
        "commands": [],
        "hooks": ["prompt/context"],
        "tools": [{
            "name": "discord_send",
            "description": "Send text to the owner-configured Discord channel.",
            "parameters": {
                "type": "object",
                "properties": {"content": {"type": "string"}},
                "required": ["content"]
            }
        }, {
            "name": "discord_send_ui",
            "description": "Send a validated Discord Components V2 document authored by Gray.",
            "parameters": {
                "type": "object",
                "properties": {
                    "document": {"type": "object"},
                    "interaction_id": {"type": "string"}
                },
                "required": ["document"]
            }
        }, {
            "name": "discord_open_modal",
            "description": "Open a validated modal in the current Discord interaction.",
            "parameters": {
                "type": "object",
                "properties": {
                    "document": {"type": "object"},
                    "interaction_id": {"type": "string"}
                },
                "required": ["document", "interaction_id"]
            }
        }, {
            "name": "discord_file",
            "description": "Import, inspect, or safely expose a generated file through Gray's private Discord media store.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["import", "metadata", "path"]},
                    "path": {"type": "string", "description": "Generated file path; import action only"},
                    "file_id": {"type": "string", "description": "Managed file ID; metadata/path actions only"},
                    "conversation": {"type": "string", "description": "Conversation key used to scope a safe path"}
                }
            }
        }, {
            "name": "discord_ui_schema",
            "description": "Return the Gray-owned Components V2 document schema and limits.",
            "parameters": {
                "type": "object",
                "properties": {"surface": {"type": "string", "enum": ["message", "modal"]}}
            }
        }]
    })
});

fn is_error() -> Value {
    json!({
        "content": "Discord delivery failed. Run gray-discord doctor; do not blindly retry partial sends.",
        "is_error": true
    })
}

/// A document or file the model can fix. Nothing reached Discord, and the
/// reason names only the model's own input (never config or token text), so
/// it is safe to hand back. "Delivery failed" sent the model to `doctor` for
/// what was a typo in its own document.
fn rejected(reason: impl std::fmt::Display) -> Value {
    json!({
        "content": format!(
            "Not sent: {reason}. Nothing reached Discord; fix it and call again \
             (discord_ui_schema shows the document shape)."
        ),
        "is_error": true
    })
}

/// Parse a model-authored document. The tool already names the surface, so a
/// missing `surface` tag is filled in rather than rejected.
fn parse_document(document: &Value, surface: &str) -> Result<DocumentRequest, Value> {
    let mut document = document.clone();
    if let Some(map) = document.as_object_mut() {
        map.entry("surface").or_insert_with(|| json!(surface));
    }
    if has_numeric_type(&document) {
        return Err(rejected(
            "component `type` is a name like \"text\", \"media_gallery\" or \"file\", \
             never a Discord number",
        ));
    }
    serde_json::from_value(json!({"document": document, "origin": "agent"}))
        .map_err(|e| rejected(format!("invalid document: {e}")))
}

/// serde reads a numeric enum tag as the *variant index*, so Discord's
/// `"type": 12` (media gallery) silently parsed as our 12th variant, a
/// separator. Numbers are refused instead of reinterpreted.
fn has_numeric_type(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("type").is_some_and(Value::is_number) || map.values().any(has_numeric_type)
        }
        Value::Array(items) => items.iter().any(has_numeric_type),
        _ => false,
    }
}

/// Dispatch one protocol method. Never echoes config/token text: every
/// failure path returns the fixed `is_error` shape above.
pub async fn dispatch(method: &str, params: &Value, config_path: &Path) -> Value {
    if method == "plugin/manifest" {
        return MANIFEST.clone();
    }
    if method == "prompt/context" {
        return json!({
            "text": "discord_send sends text; discord_send_ui and discord_open_modal send validated Components V2; discord_file imports private work files; discord_ui_schema explains the typed document. Never send secrets."
        });
    }
    if method == "tool/call" {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        return match name {
            "discord_send" => send_text(params, config_path).await,
            "discord_send_ui" => send_ui(params, config_path).await,
            "discord_open_modal" => open_modal(params, config_path).await,
            "discord_file" => import_file(params, config_path),
            "discord_ui_schema" => ui_schema(params),
            _ => is_error(),
        };
    }
    json!({"error": "Unsupported method"})
}

async fn send_text(params: &Value, config_path: &Path) -> Value {
    let content = params
        .get("args")
        .and_then(|a| a.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let config = match crate::config::load_config(config_path) {
        Ok(c) => c,
        Err(_) => return is_error(),
    };
    let Some(token) = config.get("token").and_then(Value::as_str) else {
        return is_error();
    };
    let channel = config
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let rest = crate::transport::Rest::production(token);
    let send = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        rest.rest_send(channel, content),
    )
    .await;
    match send {
        Ok(Ok(_)) => json!({"content": "Sent to the configured Discord channel."}),
        _ => is_error(),
    }
}

async fn send_ui(params: &Value, config_path: &Path) -> Value {
    let Some(args) = params.get("args") else {
        return is_error();
    };
    let Some(document) = args.get("document") else {
        return is_error();
    };
    let config = match crate::config::load_config(config_path) {
        Ok(c) => c,
        Err(_) => return is_error(),
    };
    let Some(token) = config.get("token").and_then(Value::as_str) else {
        return is_error();
    };
    let channel = config
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let store = match Store::new(&parent.join("queue.sqlite")) {
        Ok(store) => store,
        Err(_) => return is_error(),
    };
    let event_item =
        args.get("interaction_id")
            .and_then(Value::as_str)
            .and_then(|interaction_id| {
                store
                    .get(&format!("component:{interaction_id}"))
                    .ok()
                    .flatten()
            });
    let owner = event_item.as_ref().and_then(event_owner).or_else(|| {
        config
            .get("owner_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let Some(owner) = owner else {
        return is_error();
    };
    if event_item.is_none()
        && args
            .get("user_id")
            .and_then(Value::as_str)
            .is_some_and(|requested| requested != owner)
    {
        return is_error();
    }
    let request = match parse_document(document, "message") {
        Ok(request) => request,
        Err(reply) => return reply,
    };
    if request.origin != Origin::Agent {
        return is_error();
    }
    if matches!(request.document, UiDocument::Modal(_)) {
        return rejected("a modal opens with discord_open_modal, not discord_send_ui");
    }
    let files = match FileStore::new(
        store.clone(),
        parent.join("media"),
        std::env::current_dir().unwrap_or_else(|_| parent.to_path_buf()),
    ) {
        Ok(files) => files,
        Err(_) => return is_error(),
    };
    let document_id = request.document.document_id().0.clone();
    if let Err(e) = store.create_document(crate::component_state::NewDocument {
        document_id: document_id.clone(),
        owner_id: owner.to_string(),
        guild_id: None,
        channel_id: channel.to_string(),
        message_id: None,
        modal_id: None,
        surface: "message".to_string(),
        revision: 1,
        protocol_version: 1,
        ttl_secs: 3600,
    }) {
        return rejected(e);
    }
    let mut allocator = StoreAllocator::new(&store, &files, &document_id, owner.clone(), channel);
    allocator.one_shot = !matches!(
        request.document,
        UiDocument::Message(ref document) if document.lifecycle == crate::component_protocol::Lifecycle::Repeatable
    );
    seed_component_options(&mut allocator, &request.document);
    allocator.premium_enabled = config
        .get("premium")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    allocator.allowed_skus = config
        .get("premium_skus")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let compiled = match &request.document {
        UiDocument::Message(document) => compile_message(
            document,
            &mut CompileContext::new(Origin::Agent, &mut allocator),
        ),
        UiDocument::Modal(_) => return is_error(),
    };
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(e) => {
            let _ = store.invalidate_document(&document_id);
            return rejected(e);
        }
    };
    let file_refs = match collect_file_refs(&request.document, &files, &owner) {
        Ok(refs) => refs,
        Err(e) => return rejected(e),
    };
    let rest = crate::transport::Rest::production(token);
    let result = if args.get("interaction_id").and_then(Value::as_str).is_some() {
        let Some(item) = event_item else {
            return is_error();
        };
        if !event_owner_matches(&item, &owner) {
            return is_error();
        }
        let (Some(interaction_token), Some(app_id)) = (item.interaction_token, item.app_id) else {
            return is_error();
        };
        rest.edit_original_compiled(&app_id, &interaction_token, &compiled, false)
            .await
            .map(|_| ())
    } else {
        let channel_id = match channel.parse::<u64>() {
            Ok(channel) => channel,
            Err(_) => return is_error(),
        };
        rest.send_compiled(channel_id, &compiled, &file_refs, &files, &owner, None)
            .await
            .map(|message_id| {
                let _ = store.bind_message(&document_id, &message_id);
            })
    };
    match result {
        Ok(()) => json!({"content": "Sent a validated Discord Components V2 document."}),
        Err(_) => is_error(),
    }
}

async fn open_modal(params: &Value, config_path: &Path) -> Value {
    let Some(args) = params.get("args") else {
        return is_error();
    };
    let (Some(document), Some(interaction_id)) = (
        args.get("document"),
        args.get("interaction_id").and_then(Value::as_str),
    ) else {
        return is_error();
    };
    let config = match crate::config::load_config(config_path) {
        Ok(c) => c,
        Err(_) => return is_error(),
    };
    let Some(token) = config.get("token").and_then(Value::as_str) else {
        return is_error();
    };
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let store = match Store::new(&parent.join("queue.sqlite")) {
        Ok(store) => store,
        Err(_) => return is_error(),
    };
    let queue_id = format!("component:{interaction_id}");
    let Some(item) = store.get(&queue_id).ok().flatten() else {
        return is_error();
    };
    let owner: Option<String> = event_owner(&item).or_else(|| {
        config
            .get("owner_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let Some(owner) = owner else {
        return is_error();
    };
    if !event_owner_matches(&item, &owner) {
        return is_error();
    }
    let (Some(interaction_token), Some(_app_id)) = (item.interaction_token, item.app_id) else {
        return is_error();
    };
    let request = match parse_document(document, "modal") {
        Ok(request) => request,
        Err(reply) => return reply,
    };
    if request.origin != Origin::Agent {
        return is_error();
    }
    let UiDocument::Modal(modal) = &request.document else {
        return rejected("discord_open_modal takes a modal document (surface \"modal\")");
    };
    let document_id = request.document.document_id().0.clone();
    if store
        .create_document(crate::component_state::NewDocument {
            document_id: document_id.clone(),
            owner_id: owner.to_string(),
            guild_id: None,
            channel_id: item.channel.clone(),
            message_id: None,
            modal_id: Some(item.id.clone()),
            surface: "modal".to_string(),
            revision: 1,
            protocol_version: 1,
            ttl_secs: 3600,
        })
        .is_err()
    {
        return is_error();
    }
    let files = match FileStore::new(
        store.clone(),
        parent.join("media"),
        std::env::current_dir().unwrap_or_else(|_| parent.to_path_buf()),
    ) {
        Ok(files) => files,
        Err(_) => return is_error(),
    };
    let mut allocator = StoreAllocator::new(&store, &files, &document_id, owner, &item.channel);
    allocator.one_shot = !matches!(
        modal.lifecycle,
        crate::component_protocol::Lifecycle::Repeatable
    );
    seed_modal_options(&mut allocator, modal);
    let compiled = match compile_modal(
        modal,
        &mut CompileContext::new(Origin::Agent, &mut allocator),
    ) {
        Ok(compiled) => compiled,
        Err(e) => return rejected(e),
    };
    let rest = crate::transport::Rest::production(token);
    match rest
        .open_modal(interaction_id, &interaction_token, &compiled)
        .await
    {
        Ok(()) => {
            let _ = store.bind_modal(&document_id, interaction_id);
            json!({"content": "Opened a validated Discord modal."})
        }
        Err(_) => is_error(),
    }
}

fn import_file(params: &Value, config_path: &Path) -> Value {
    let Some(args) = params.get("args") else {
        return is_error();
    };
    let config = match crate::config::load_config(config_path) {
        Ok(c) => c,
        Err(_) => return is_error(),
    };
    let requested_owner = args
        .get("user_id")
        .and_then(Value::as_str)
        .or_else(|| config.get("owner_id").and_then(Value::as_str));
    let Some(owner) = requested_owner else {
        return is_error();
    };
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let store = match Store::new(&parent.join("queue.sqlite")) {
        Ok(store) => store,
        Err(_) => return is_error(),
    };
    let files_root = std::env::current_dir().unwrap_or_else(|_| parent.to_path_buf());
    let files = match FileStore::new(store, parent.join("media"), files_root.clone()) {
        Ok(files) => files,
        Err(_) => return is_error(),
    };
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("import");
    match action {
        "import" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return is_error();
            };
            match files.import_generated(owner, Path::new(path)) {
                Ok(file) => json!({
                    "content": format!(
                        "Imported a private managed Discord file. file_id={} ({}). \
                         Show it with a discord_send_ui component like \
                         {{\"type\": \"media_gallery\", \"items\": [{{\"media\": \
                         {{\"kind\": \"file\", \"file_id\": \"{}\"}}}}]}}.",
                        file.id, file.name, file.id
                    ),
                    "file": {
                        "file_id": file.id,
                        "name": file.name,
                        "media_type": file.media_type,
                        "size": file.size,
                        "sha256": file.sha256
                    }
                }),
                Err(e) => rejected(format!(
                    "{e}; save generated files under {} and import that path",
                    files_root.display()
                )),
            }
        }
        "metadata" => {
            let Some(file_id) = args.get("file_id").and_then(Value::as_str) else {
                return is_error();
            };
            match files.metadata(owner, file_id) {
                Ok(file) => json!({"file": {
                    "file_id": file.id,
                    "name": file.name,
                    "media_type": file.media_type,
                    "size": file.size,
                    "sha256": file.sha256,
                    "expires_at": file.expires_at
                }}),
                Err(_) => is_error(),
            }
        }
        "path" => {
            let (Some(file_id), Some(conversation)) = (
                args.get("file_id").and_then(Value::as_str),
                args.get("conversation").and_then(Value::as_str),
            ) else {
                return is_error();
            };
            match files.open_for_conversation(owner, conversation, file_id) {
                Ok(path) => json!({"path": path.to_string_lossy()}),
                Err(_) => is_error(),
            }
        }
        _ => is_error(),
    }
}

fn seed_component_options(allocator: &mut StoreAllocator<'_>, document: &UiDocument) {
    if let UiDocument::Message(message) = document {
        seed_message_nodes(allocator, &message.components);
    }
}

fn seed_message_nodes(allocator: &mut StoreAllocator<'_>, nodes: &[MessageNode]) {
    for node in nodes {
        match node {
            MessageNode::StringSelect(select) => {
                if let Ok(options) = serde_json::to_value(&select.options) {
                    allocator.set_component_options(&select.logical_id.0, options);
                }
            }
            MessageNode::ActionRow(row) => seed_message_nodes(allocator, &row.children),
            MessageNode::Container(container) => {
                seed_message_nodes(allocator, &container.components)
            }
            MessageNode::Section(_) => {}
            _ => {}
        }
    }
}

fn seed_modal_options(
    allocator: &mut StoreAllocator<'_>,
    modal: &crate::component_protocol::ModalDocument,
) {
    for node in &modal.components {
        match node {
            crate::component_protocol::ModalNode::Label(label) => {
                seed_modal_control(allocator, &label.component)
            }
            crate::component_protocol::ModalNode::ActionRow(row) => {
                for control in &row.children {
                    seed_modal_control(allocator, control);
                }
            }
        }
    }
}

fn seed_modal_control(
    allocator: &mut StoreAllocator<'_>,
    control: &crate::component_protocol::ModalControl,
) {
    if let crate::component_protocol::ModalControl::StringSelect(select) = control {
        if let Ok(options) = serde_json::to_value(&select.options) {
            allocator.set_component_options(&select.logical_id.0, options);
        }
    }
}

fn event_owner(item: &crate::durable::InboxItem) -> Option<String> {
    item.input_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| {
            value
                .pointer("/payload/user_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn event_owner_matches(item: &crate::durable::InboxItem, owner: &str) -> bool {
    event_owner(item).is_some_and(|user| user == owner)
}

fn ui_schema(params: &Value) -> Value {
    let surface = params
        .get("args")
        .and_then(|args| args.get("surface"))
        .and_then(Value::as_str)
        .unwrap_or("message");
    let components = if surface == "modal" {
        json!([
            {"type": "label", "required": ["label", "component"], "component_types": [4, 3, 5, 6, 7, 8, 19, 21, 22, 23]},
            {"type": "action_row", "required": ["children"], "max_items": 5}
        ])
    } else {
        // `type` is always one of these names: the numeric Discord codes are
        // compiler-owned, and listing them here taught the model to send them.
        json!([
            {"type": "text", "required": ["content"]},
            {"type": "action_row", "required": ["children"], "max_items": 5,
             "child_types": ["button", "string_select", "user_select", "role_select", "mentionable_select", "channel_select"]},
            {"type": "button", "required": ["style"], "optional": ["logical_id", "label", "url", "emoji", "disabled"],
             "style": ["primary", "secondary", "success", "danger", "link"]},
            {"type": "string_select", "required": ["logical_id", "options"], "option": {"required": ["label", "value"]}},
            {"type": "section", "required": ["children", "accessory"], "children": "text components",
             "accessory": "a button or a thumbnail"},
            {"type": "thumbnail", "required": ["media"]},
            {"type": "media_gallery", "required": ["items"], "item": {"required": ["media"], "optional": ["description", "spoiler"]}},
            {"type": "file", "required": ["media"], "optional": ["name"]},
            {"type": "separator", "optional": ["divider", "spacing"]},
            {"type": "container", "required": ["components"], "optional": ["accent_color", "spoiler"]}
        ])
    };
    let mut reply = json!({
        "protocol": "gray.discord.ui",
        "version": 1,
        "surface": surface,
        "document": {"surface": surface, "version": 1, "document_id": "stable logical id", "components": components},
        "media": [
            {"kind": "file", "file_id": "from discord_file import"},
            {"kind": "remote", "url": "https://…"}
        ],
        "example": if surface == "modal" { Value::Null } else { json!({
            "surface": "message", "version": 1, "document_id": "plunger-1",
            "components": [
                {"type": "text", "content": "here it is"},
                {"type": "media_gallery", "items": [{"media": {"kind": "file", "file_id": "<id>"}}]}
            ]
        }) },
        "limits": {
            "max_components": 40,
            "max_action_row_items": 5,
            "max_string_select_options": 25,
            "max_radio_options": 10,
            "max_checkbox_options": 10,
            "max_modal_title_chars": 45
        },
        "notes": [
            "interactive controls belong inside action_row",
            "file_id values come only from discord_file",
            "custom_id and Discord numeric type values are compiler-owned"
        ]
    });
    // The tool protocol requires `content`, and gray hands the model only that
    // field. Without it the host rejects the whole reply, so the schema is
    // unreachable exactly when someone asks for it.
    let rendered = serde_json::to_string_pretty(&reply).unwrap_or_default();
    reply["content"] = json!(format!("{surface} surface schema:\n{rendered}"));
    reply
}

fn collect_file_refs(
    document: &UiDocument,
    files: &FileStore,
    owner: &str,
) -> Result<Vec<FileRef>, String> {
    let value =
        serde_json::to_value(document).map_err(|_| "document cannot be inspected".to_string())?;
    let mut ids = Vec::new();
    collect_file_ids(&value, &mut ids);
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .map(|id| {
            files.metadata(owner, &id).map(|metadata| FileRef {
                id: metadata.id,
                name: metadata.name,
                media_type: metadata.media_type,
                size: metadata.size,
                sha256: metadata.sha256,
            })
        })
        .collect()
}

fn collect_file_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if map.get("kind").and_then(Value::as_str) == Some("file") {
                if let Some(id) = map.get("file_id").and_then(Value::as_str) {
                    ids.push(id.to_string());
                }
            }
            for child in map.values() {
                collect_file_ids(child, ids);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_file_ids(value, ids);
            }
        }
        _ => {}
    }
}

/// Blocking stdin loop. Exits 0 on EOF/`plugin/shutdown`, 1 on wire overflow.
pub fn serve(config_path: &Path) -> ! {
    use std::io::{BufRead, Write};
    const LIMIT: usize = 256 * 1024;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sidecar runtime builds");
    loop {
        let mut buf: Vec<u8> = Vec::new();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(_) => std::process::exit(1),
        };
        if n == 0 {
            std::process::exit(0);
        }
        if buf.len() > LIMIT {
            std::process::exit(1);
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !req.is_object() {
            continue;
        }
        if req.get("method").and_then(Value::as_str) == Some("plugin/shutdown") {
            std::process::exit(0);
        }
        let id = match req.get("id") {
            Some(Value::Number(_)) => req.get("id").cloned().unwrap(),
            _ => continue,
        };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = match req.get("params") {
            Some(Value::Object(_)) => req.get("params").cloned().unwrap(),
            _ => json!({}),
        };
        let result = rt.block_on(dispatch(method, &params, config_path));
        let row = json!({"id": id, "result": result});
        if writeln!(out, "{row}").is_err() {
            std::process::exit(1);
        }
        if out.flush().is_err() {
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe the model sent: valid in every field but the `surface` tag,
    /// which the tool already implies.
    #[test]
    fn a_document_without_surface_parses_for_its_tool() {
        let probe = json!({
            "components": [{"type": "text", "content": "probe: ui path"}],
            "document_id": "probe-1",
            "version": 1
        });
        let request = parse_document(&probe, "message").expect("parses");
        assert!(matches!(request.document, UiDocument::Message(_)));
        // An explicit tag still wins, so a modal sent to send_ui stays a modal.
        let modal = json!({"surface": "modal", "title": "t", "components": [],
                           "document_id": "m", "version": 1});
        let request = parse_document(&modal, "message").expect("parses");
        assert!(matches!(request.document, UiDocument::Modal(_)));
    }

    /// `"type": 12` is Discord's media gallery but our 12th variant is a
    /// separator; it must be refused, not reinterpreted.
    #[test]
    fn a_numeric_component_type_is_refused() {
        let doc = json!({"version": 1, "document_id": "d",
                         "components": [{"type": 12}]});
        let reply = parse_document(&doc, "message").expect_err("refused");
        assert!(reply["content"]
            .as_str()
            .unwrap()
            .contains("never a Discord number"));
    }
}
