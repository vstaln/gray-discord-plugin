//! Port of gray_discord/sidecar.py: NDJSON protocol 1.1 sidecar.
//! Manifest answers without config; delivery failures never echo secrets.
use serde_json::{json, Value};
use std::path::Path;

use crate::component_compile::{compile_message, compile_modal, CompileContext};
use crate::component_media::FileStore;
use crate::component_protocol::{
    DocumentRequest, FileRef, MessageNode, Origin, Surface, UiDocument,
};
use crate::component_state::StoreAllocator;
use crate::durable::Store;
use std::sync::LazyLock;

/// JSON Schema for a message document, shared by the manifest. Kept
/// descriptive rather than strict: `component_normalize` accepts common
/// dialects and the compiler is the real gate.
fn message_document_schema() -> Value {
    json!({
        "type": "object",
        "description": "Components V2 message. Minimal: {\"components\":[{\"type\":\"text\",\"content\":\"**hi**\"}]}. Call discord_ui_schema for every field and ready-made examples.",
        "properties": {
            "document_id": {"type": "string", "description": "Optional stable id; generated when omitted"},
            "title": {"type": "string", "description": "Optional shortcut: rendered as a '## title' heading"},
            "accent_color": {"type": ["string", "integer"], "description": "Optional shortcut: '#5865F2' wraps everything in one accented container card"},
            "visibility": {"type": "string", "enum": ["public", "ephemeral"]},
            "lifecycle": {"type": "string", "enum": ["static", "one_shot", "repeatable", "editable"]},
            "components": {
                "type": "array",
                "description": "Top-level components, in order",
                "items": {
                    "type": "object",
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": ["text", "container", "section", "media_gallery", "file", "separator", "action_row", "button", "string_select", "user_select", "role_select", "mentionable_select", "channel_select"]
                        },
                        "content": {"type": "string", "description": "text: Discord markdown (# heading, **bold**, -# subtext, lists, code blocks)"},
                        "components": {"type": "array", "description": "container: child components", "items": {"type": "object"}},
                        "accent_color": {"type": ["string", "integer"], "description": "container: '#RRGGBB' or integer"},
                        "children": {"type": "array", "description": "action_row: buttons/select; section: 1-3 text strings", "items": {}},
                        "accessory": {"type": "object", "description": "section: {\"type\":\"thumbnail\",\"media\":URL_OR_FILE_ID} or a button"},
                        "items": {"type": "array", "description": "media_gallery: 1-10 of {\"media\": URL_OR_FILE_ID, \"description\"?}", "items": {}},
                        "media": {"description": "URL string, managed file_id string, {\"url\"}, or {\"file_id\"}"},
                        "spacing": {"type": "string", "enum": ["small", "large"]},
                        "divider": {"type": "boolean"},
                        "label": {"type": "string"},
                        "style": {"type": "string", "enum": ["primary", "secondary", "success", "danger", "link"]},
                        "url": {"type": "string"},
                        "logical_id": {"type": "string"},
                        "options": {"type": "array", "items": {}}
                    },
                    "required": ["type"]
                }
            }
        },
        "required": ["components"]
    })
}

/// Static manifest (`discord` 0.1.0, protocol 1.1, no commands).
pub static MANIFEST: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "name": "discord",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "commands": [],
        "hooks": ["prompt/context"],
        "tools": [{
            "name": "discord_send",
            "label": "Discord Send",
            "description": "Send markdown text to the owner's Discord channel. Put MEDIA:/absolute/path.png on its own line to upload files with it (images show as a gallery).",
            "parameters": {
                "type": "object",
                "properties": {"content": {"type": "string"}},
                "required": ["content"]
            }
        }, {
            "name": "discord_send_ui",
            "label": "Discord Send UI",
            "preview": "document.title",
            "description": "Send a rich Discord Components V2 message (cards, galleries, sections, buttons, selects). Pass the document as a JSON object. For local files, prefer discord_file action=send, or import with discord_file and reference the returned file_id as media.",
            "parameters": {
                "type": "object",
                "properties": {
                    "document": message_document_schema(),
                    "interaction_id": {"type": "string", "description": "Only when answering a Discord component interaction"}
                },
                "required": ["document"]
            }
        }, {
            "name": "discord_open_modal",
            "label": "Discord Open Modal",
            "preview": "document.title",
            "description": "Open a modal form in response to a Discord component interaction. Call discord_ui_schema surface=modal for the shape.",
            "parameters": {
                "type": "object",
                "properties": {
                    "document": {
                        "type": "object",
                        "description": "{\"title\":\"Feedback\",\"components\":[{\"type\":\"label\",\"label\":\"Why?\",\"component\":{\"type\":\"text_input\",\"logical_id\":\"why\",\"style\":\"paragraph\"}}]}"
                    },
                    "interaction_id": {"type": "string"}
                },
                "required": ["document", "interaction_id"]
            }
        }, {
            "name": "discord_file",
            "label": "Discord File",
            "description": "Post or stage local files for Discord. action=send uploads files straight to the channel (images in a gallery, others as file cards) with an optional caption. action=import stores one file privately and returns a file_id for discord_send_ui media. Any readable path works except credentials/config files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["send", "import", "metadata", "path"], "description": "Default: send when caption/paths given, else import"},
                    "path": {"type": "string", "description": "File path (absolute, ~/..., or relative to the working directory)"},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "send only: up to 10 files in one message"},
                    "caption": {"type": "string", "description": "send only: markdown shown above the files"},
                    "file_id": {"type": "string", "description": "Managed file ID; metadata/path actions only"},
                    "conversation": {"type": "string", "description": "Conversation key used to scope a safe path"}
                }
            }
        }, {
            "name": "discord_ui_schema",
            "label": "Discord UI Schema",
            "description": "Return the full Components V2 document reference: every component's fields, Discord's limits, and copy-ready examples.",
            "parameters": {
                "type": "object",
                "properties": {"surface": {"type": "string", "enum": ["message", "modal"]}}
            }
        }]
    })
});

const PROMPT_CONTEXT: &str = "Discord tools: discord_send posts markdown; add a line MEDIA:/absolute/path to attach a file. discord_file action=send posts local files (images become a gallery) with a caption. discord_send_ui posts rich Components V2 documents: wrap related content in a container with an accent_color, lead with a '## heading', use '-# ' for subtle footnotes, separators between sections, a section+thumbnail for an item with an image, media_gallery for pictures, and action_row buttons for links. discord_ui_schema has examples. Errors say exactly what to fix; nothing is sent when a document is invalid. Never send secrets.";

fn is_error() -> Value {
    json!({
        "content": "Discord delivery failed. Run gray-discord doctor; do not blindly retry partial sends.",
        "is_error": true
    })
}

/// A specific, fixable failure. `detail` must never contain config values.
fn not_sent(detail: impl std::fmt::Display) -> Value {
    json!({
        "content": format!("Not sent: {detail}. Nothing reached Discord; fix it and call again (discord_ui_schema has the shape and examples)."),
        "is_error": true
    })
}

/// Failure after Discord was contacted; the detail is Discord's own reason.
fn rejected(error: &crate::transport::TransportError) -> Value {
    json!({
        "content": format!("Not sent: {error}. Nothing was posted; fix the document and call again."),
        "is_error": true
    })
}

/// Dispatch one protocol method. Never echoes config/token text: failures
/// describe the caller's own input or Discord's validation response only.
pub async fn dispatch(method: &str, params: &Value, config_path: &Path) -> Value {
    if method == "plugin/manifest" {
        return MANIFEST.clone();
    }
    if method == "prompt/context" {
        return json!({"text": PROMPT_CONTEXT});
    }
    if method == "tool/call" {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        return match name {
            "discord_send" => send_text(params, config_path).await,
            "discord_send_ui" => send_ui(params, config_path).await,
            "discord_open_modal" => open_modal(params, config_path).await,
            "discord_file" => file_tool(params, config_path).await,
            "discord_ui_schema" => ui_schema(params),
            _ => is_error(),
        };
    }
    json!({"error": "Unsupported method"})
}

/// The calling session's working directory, falling back to the sidecar's.
fn session_cwd(params: &Value) -> std::path::PathBuf {
    params
        .pointer("/session/cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|cwd| cwd.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(std::env::temp_dir)
}

/// Token and channel from config; `None` means the bridge is not set up.
fn delivery_target(config_path: &Path) -> Result<(Value, String, u64), Value> {
    let config = crate::config::load_config(config_path).map_err(|_| is_error())?;
    let token = config
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(is_error)?
        .to_string();
    let channel = config
        .get("channel_id")
        .and_then(Value::as_str)
        .and_then(|channel| channel.parse::<u64>().ok())
        .ok_or_else(is_error)?;
    Ok((config, token, channel))
}

async fn send_text(params: &Value, config_path: &Path) -> Value {
    let content = params
        .get("args")
        .and_then(|a| a.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if content.trim().is_empty() {
        return not_sent("content is empty");
    }
    let (config, token, channel) = match delivery_target(config_path) {
        Ok(target) => target,
        Err(error) => return error,
    };
    let roots = crate::media_tags::roots_from_config(&config);
    let (prose, media) = crate::media_tags::extract(content, &session_cwd(params), &roots);
    let rest = crate::transport::Rest::production(&token);
    if !prose.trim().is_empty() {
        let send = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            rest.rest_send(&channel.to_string(), &prose),
        )
        .await;
        match send {
            Ok(Ok(_)) => {}
            Ok(Err(error @ crate::transport::TransportError::Http(400, _))) => {
                return rejected(&error)
            }
            _ => return is_error(),
        }
    }
    if !media.is_empty() {
        if let Err(error) = upload(&rest, channel, &media, None).await {
            return error;
        }
    }
    let files = if media.is_empty() {
        String::new()
    } else {
        format!(" with {} file(s)", media.len())
    };
    json!({"content": format!("Sent to the configured Discord channel{files}.")})
}

/// Upload local files in batches of ten as V2 gallery/file messages.
async fn upload(
    rest: &crate::transport::Rest,
    channel: u64,
    paths: &[std::path::PathBuf],
    caption: Option<&str>,
) -> Result<usize, Value> {
    let uploads = crate::media_tags::load(paths);
    if uploads.is_empty() {
        return Err(not_sent("no readable files to upload"));
    }
    let mut sent = 0;
    for (index, batch) in uploads
        .chunks(crate::media_tags::MAX_UPLOADS_PER_MESSAGE)
        .enumerate()
    {
        let components =
            crate::media_tags::components(batch, if index == 0 { caption } else { None });
        let send = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            rest.send_v2_uploads(channel, &components, batch, None),
        )
        .await;
        match send {
            Ok(Ok(_)) => sent += batch.len(),
            Ok(Err(error @ crate::transport::TransportError::Http(400 | 413, _))) => {
                return Err(rejected(&error))
            }
            _ if sent > 0 => {
                return Err(json!({
                    "content": format!("Partially sent: {sent} file(s) reached Discord before a failure; do not resend those."),
                    "is_error": true
                }))
            }
            _ => return Err(is_error()),
        }
    }
    Ok(sent)
}

async fn send_ui(params: &Value, config_path: &Path) -> Value {
    let Some(args) = params.get("args") else {
        return is_error();
    };
    let Some(document) = args.get("document") else {
        return not_sent("missing the `document` argument");
    };
    let request = match parse_request(document, Surface::Message) {
        Ok(request) => request,
        Err(error) => return error,
    };
    if matches!(request.document, UiDocument::Modal(_)) {
        return not_sent("this is a modal document; open it with discord_open_modal");
    }
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
        Err(error) => {
            let _ = store.invalidate_document(&document_id);
            return not_sent(format!("invalid document: {error}"));
        }
    };
    let file_refs = match collect_file_refs(&request.document, &files, &owner) {
        Ok(refs) => refs,
        Err(error) => {
            let _ = store.invalidate_document(&document_id);
            return not_sent(format!(
                "{error}; file_id values must come from discord_file action=import"
            ));
        }
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
        Ok(()) => json!({
            "content": format!("Sent a Discord Components V2 message (document_id {document_id}).")
        }),
        Err(error @ crate::transport::TransportError::Http(400 | 413, _)) => {
            let _ = store.invalidate_document(&document_id);
            rejected(&error)
        }
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
        return not_sent("discord_open_modal needs both `document` and `interaction_id`");
    };
    let request = match parse_request(document, Surface::Modal) {
        Ok(request) => request,
        Err(error) => return error,
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
    let UiDocument::Modal(modal) = &request.document else {
        return not_sent("this is a message document; send it with discord_send_ui");
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
        Err(error) => return not_sent(format!("invalid modal: {error}")),
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
        Err(error @ crate::transport::TransportError::Http(400, _)) => rejected(&error),
        Err(_) => is_error(),
    }
}

async fn file_tool(params: &Value, config_path: &Path) -> Value {
    let Some(args) = params.get("args") else {
        return not_sent("missing arguments");
    };
    let cwd = session_cwd(params);
    let mut raw_paths: Vec<String> = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        raw_paths.insert(0, path.to_string());
    }
    let action = args.get("action").and_then(Value::as_str).unwrap_or(
        if args.get("caption").is_some() || raw_paths.len() > 1 {
            "send"
        } else {
            "import"
        },
    );
    if action == "send" {
        if raw_paths.is_empty() {
            return not_sent("action=send needs `path` or `paths`");
        }
        let (config, token, channel) = match delivery_target(config_path) {
            Ok(target) => target,
            Err(error) => return error,
        };
        let roots = crate::media_tags::roots_from_config(&config);
        let mut paths = Vec::new();
        for raw in &raw_paths {
            match resolve_local(raw, &cwd, &roots) {
                Ok(path) => paths.push(path),
                Err(error) => return not_sent(error),
            }
        }
        let rest = crate::transport::Rest::production(&token);
        let caption = args.get("caption").and_then(Value::as_str);
        return match upload(&rest, channel, &paths, caption).await {
            Ok(count) => {
                json!({"content": format!("Posted {count} file(s) to the configured Discord channel.")})
            }
            Err(error) => error,
        };
    }

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
    let files = match FileStore::new(store, parent.join("media"), cwd.clone()) {
        Ok(files) => files,
        Err(_) => return is_error(),
    };
    match action {
        "import" => {
            let Some(raw) = raw_paths.first() else {
                return not_sent("action=import needs `path`");
            };
            let roots = crate::media_tags::roots_from_config(&config);
            let path = match resolve_local(raw, &cwd, &roots) {
                Ok(path) => path,
                Err(error) => return not_sent(error),
            };
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file")
                .to_string();
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => return not_sent(format!("cannot read {}", path.display())),
            };
            let media_type = crate::component_media::infer_media_type(&name);
            match files.import_bytes(owner, &name, media_type, &bytes) {
                Ok(file) => {
                    let media = if media_type.starts_with("image/")
                        || media_type.starts_with("video/")
                    {
                        json!({"type": "media_gallery", "items": [{"media": {"file_id": file.id}}]})
                    } else {
                        json!({"type": "file", "media": {"file_id": file.id}})
                    };
                    json!({
                        "content": format!(
                            "Imported {} as file_id {}. It is private until you send it: discord_send_ui document={}. Or skip importing and use discord_file action=send.",
                            file.name,
                            file.id,
                            json!({"components": [media]})
                        ),
                        "file": {
                            "file_id": file.id,
                            "name": file.name,
                            "media_type": file.media_type,
                            "size": file.size,
                            "sha256": file.sha256
                        }
                    })
                }
                Err(error) => not_sent(error),
            }
        }
        "metadata" => {
            let Some(file_id) = args.get("file_id").and_then(Value::as_str) else {
                return not_sent("action=metadata needs `file_id`");
            };
            match files.metadata(owner, file_id) {
                Ok(file) => json!({
                    "content": format!("{} ({}, {} bytes)", file.name, file.media_type, file.size),
                    "file": {
                        "file_id": file.id,
                        "name": file.name,
                        "media_type": file.media_type,
                        "size": file.size,
                        "sha256": file.sha256,
                        "expires_at": file.expires_at
                    }
                }),
                Err(error) => not_sent(error),
            }
        }
        "path" => {
            let (Some(file_id), Some(conversation)) = (
                args.get("file_id").and_then(Value::as_str),
                args.get("conversation").and_then(Value::as_str),
            ) else {
                return not_sent("action=path needs `file_id` and `conversation`");
            };
            match files.open_for_conversation(owner, conversation, file_id) {
                Ok(path) => json!({
                    "content": path.to_string_lossy(),
                    "path": path.to_string_lossy()
                }),
                Err(error) => not_sent(error),
            }
        }
        other => not_sent(format!(
            "unknown action {other:?}; use send, import, metadata, or path"
        )),
    }
}

/// Resolve a caller path (absolute, `~/`, or relative to the session cwd)
/// and refuse credentials, config, oversized files, and anything outside
/// configured `media_roots`.
fn resolve_local(
    raw: &str,
    cwd: &Path,
    roots: &[std::path::PathBuf],
) -> Result<std::path::PathBuf, String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("MEDIA:").unwrap_or(raw).trim();
    let path = crate::media_tags::resolve(raw, cwd).ok_or_else(|| "path is empty".to_string())?;
    crate::media_tags::deliverable(&path, roots)
}

/// Coerce and parse an agent document; errors name the path to fix.
fn parse_request(document: &Value, surface: Surface) -> Result<DocumentRequest, Value> {
    let document = crate::component_normalize::coerce_document(document, surface)
        .and_then(|value| crate::component_normalize::parse_document(&value))
        .map_err(|error| not_sent(format!("invalid document: {error}")))?;
    Ok(DocumentRequest {
        document,
        origin: Origin::Agent,
    })
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
    let mut reference = if surface == "modal" {
        modal_reference()
    } else {
        message_reference()
    };
    reference["protocol"] = json!("gray.discord.ui");
    reference["version"] = json!(1);
    reference["surface"] = json!(surface);
    reference["limits"] = json!({
        "max_components": 40,
        "max_total_text_chars": 4000,
        "max_action_row_items": 5,
        "max_section_texts": 3,
        "max_gallery_items": 10,
        "max_string_select_options": 25,
        "max_radio_options": 10,
        "max_checkbox_options": 10,
        "max_button_label_chars": 80,
        "max_modal_title_chars": 45
    });
    let content = serde_json::to_string_pretty(&reference).unwrap_or_default();
    reference["content"] = json!(content);
    reference
}

fn message_reference() -> Value {
    json!({
        "how_to_call": "discord_send_ui document={\"components\":[...]} (an object; version, document_id and surface are filled in for you)",
        "document": {
            "version": 1,
            "document_id": "optional stable id",
            "title": "optional shortcut: becomes a '## title' text",
            "accent_color": "optional shortcut: '#5865F2' wraps all components in one container card",
            "visibility": "public | ephemeral (ephemeral only when answering an interaction)",
            "components": [
                {"type": "text", "content": "Discord markdown, max 4000 chars across the whole message"},
                {"type": "container", "accent_color": "#5865F2", "spoiler": false, "components": ["text | section | media_gallery | file | separator | action_row (no nested containers)"]},
                {"type": "section", "children": ["1-3 markdown strings"], "accessory": {"type": "thumbnail", "media": "https://... or file_id", "description": "alt text"}},
                {"type": "section", "children": ["text"], "accessory": {"type": "button", "label": "Open", "style": "link", "url": "https://..."}},
                {"type": "media_gallery", "items": [{"media": "https://... or file_id", "description": "alt text", "spoiler": false}]},
                {"type": "file", "media": {"file_id": "from discord_file import"}},
                {"type": "separator", "divider": true, "spacing": "small | large"},
                {"type": "action_row", "children": ["up to 5 buttons, or exactly 1 select"]},
                {"type": "button", "label": "Approve", "style": "primary | secondary | success | danger | link", "logical_id": "approve (not for link)", "url": "link only", "emoji": "✅"},
                {"type": "string_select", "logical_id": "pick", "placeholder": "Choose…", "options": [{"label": "A", "value": "a", "description": "optional", "emoji": "🅰️"}], "min_values": 1, "max_values": 1},
                {"type": "user_select | role_select | mentionable_select | channel_select", "logical_id": "who", "placeholder": "Pick someone"}
            ]
        },
        "media": "A media value is an https URL string, a file_id string, {\"url\": ...} or {\"file_id\": ...}. Local files: discord_file action=import returns a file_id; or skip all of this with discord_file action=send.",
        "markdown": "# / ## / ### headings, **bold**, *italic*, __underline__, ~~strike~~, `code`, ```lang blocks```, > quotes, - lists, [links](https://...), -# small grey subtext, ||spoiler||, <t:UNIX:R> relative timestamps.",
        "design_tips": [
            "One container per message with an accent_color reads as a polished card.",
            "Open with '## Title' then a '-# context line' (subtext) for hierarchy.",
            "Separate logical blocks with separators; use spacing large between major sections.",
            "A section with a thumbnail accessory is the best layout for an item with an image (repo, article, product).",
            "Use media_gallery for 1-10 images; Discord tiles them automatically.",
            "Put links as link-style buttons in an action_row at the bottom instead of raw URLs.",
            "Keep text short; Discord renders markdown inside text components."
        ],
        "accent_colors": {"blurple": "#5865F2", "green": "#57F287", "yellow": "#FEE75C", "red": "#ED4245", "fuchsia": "#EB459E"},
        "examples": {
            "status_card": {"components": [{"type": "container", "accent_color": "#57F287", "components": [
                {"type": "text", "content": "## ✅ Deploy finished\n-# main · 3m 12s"},
                {"type": "separator"},
                {"type": "text", "content": "**42** tests passed\n**0** failed"},
                {"type": "action_row", "children": [{"type": "button", "style": "link", "label": "View logs", "url": "https://example.com/logs"}]}
            ]}]},
            "image_post": {"components": [{"type": "container", "accent_color": "#5865F2", "components": [
                {"type": "text", "content": "## Today's render\n-# generated just now"},
                {"type": "media_gallery", "items": [{"media": "FILE_ID_FROM_discord_file", "description": "the render"}]}
            ]}]},
            "item_with_thumbnail": {"components": [{"type": "container", "components": [
                {"type": "section", "children": ["### gray-discord-plugin", "Discord bridge for Gray, now with Components V2"], "accessory": {"type": "thumbnail", "media": "https://github.com/github.png"}}
            ]}]},
            "choice": {"lifecycle": "one_shot", "components": [
                {"type": "text", "content": "**Ship it?**"},
                {"type": "action_row", "children": [
                    {"type": "button", "logical_id": "yes", "label": "Ship", "style": "success"},
                    {"type": "button", "logical_id": "no", "label": "Hold", "style": "danger"}
                ]}
            ]}
        },
        "notes": [
            "Discord numeric types and custom_id values are accepted but never needed; the compiler owns them.",
            "Buttons and selects outside an action_row are grouped into rows automatically.",
            "A thumbnail only exists as a section accessory; a standalone one becomes a gallery.",
            "Errors name the exact path (e.g. $.components[0].components[2]) and nothing is posted until the document is valid."
        ]
    })
}

fn modal_reference() -> Value {
    json!({
        "how_to_call": "discord_open_modal interaction_id=... document={\"title\":\"...\",\"components\":[...]}",
        "document": {
            "version": 1,
            "document_id": "optional stable id",
            "title": "1-45 chars, required",
            "components": [
                {"type": "label", "label": "Question, 1-45 chars", "description": "optional help, max 100", "component": {"type": "text_input", "logical_id": "answer", "style": "short | paragraph", "placeholder": "…", "required": true, "min_length": 0, "max_length": 4000}},
                {"type": "label", "label": "Pick one", "component": {"type": "string_select | radio_group | checkbox_group | checkbox | file_upload | user_select | role_select | mentionable_select | channel_select", "logical_id": "choice", "options": [{"label": "A", "value": "a"}]}},
                {"type": "action_row", "children": ["legacy: controls without a label"]}
            ]
        },
        "notes": [
            "Wrap every field in a label; a bare control with its own `label` is wrapped for you.",
            "Discord allows up to 5 top-level components in a modal."
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
