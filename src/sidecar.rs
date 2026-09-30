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
    let request: DocumentRequest =
        match serde_json::from_value(json!({"document": document, "origin": "agent"})) {
            Ok(request) => request,
            Err(_) => return is_error(),
        };
    if request.origin != Origin::Agent {
        return is_error();
    }
    if matches!(request.document, UiDocument::Modal(_)) {
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
    let document_id = request.document.document_id().0.clone();
    if store
        .create_document(crate::component_state::NewDocument {
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
        })
        .is_err()
    {
        return is_error();
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
        Err(_) => {
            let _ = store.invalidate_document(&document_id);
            return is_error();
        }
    };
    let file_refs = match collect_file_refs(&request.document, &files, &owner) {
        Ok(refs) => refs,
        Err(_) => return is_error(),
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
    let request: DocumentRequest =
        match serde_json::from_value(json!({"document": document, "origin": "agent"})) {
            Ok(request) => request,
            Err(_) => return is_error(),
        };
    if request.origin != Origin::Agent {
        return is_error();
    }
    let UiDocument::Modal(modal) = &request.document else {
        return is_error();
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
        Err(_) => return is_error(),
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
    let files = match FileStore::new(
        store,
        parent.join("media"),
        std::env::current_dir().unwrap_or_else(|_| parent.to_path_buf()),
    ) {
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
                    "content": "Imported a private managed Discord file.",
                    "file": {
                        "file_id": file.id,
                        "name": file.name,
                        "media_type": file.media_type,
                        "size": file.size,
                        "sha256": file.sha256
                    }
                }),
                Err(_) => is_error(),
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
        json!([
            {"type": "action_row", "required": ["children"], "max_items": 5, "child_types": [2, 3, 5, 6, 7, 8]},
            {"type": "text", "required": ["content"]},
            {"type": "section", "required": ["children", "accessory"]},
            {"type": 11}, {"type": 12}, {"type": 13}, {"type": 14}, {"type": 17}
        ])
    };
    json!({
        "protocol": "gray.discord.ui",
        "version": 1,
        "surface": surface,
        "document": {"version": 1, "document_id": "stable logical id", "components": components},
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
    })
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
