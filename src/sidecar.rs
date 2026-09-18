//! Port of gray_discord/sidecar.py: NDJSON protocol 1.1 sidecar.
//! Manifest answers without config; delivery failures never echo secrets.
use serde_json::{json, Value};
use std::path::Path;
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
            "text": "discord_send sends to your configured home channel; never send secrets."
        });
    }
    if method == "tool/call" {
        if params.get("name").and_then(Value::as_str) != Some("discord_send") {
            return is_error();
        }
        let content = params
            .get("args")
            .and_then(|a| a.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let config = match crate::config::load_config(config_path) {
            Ok(c) => c,
            Err(_) => return is_error(),
        };
        let token = config.get("token").and_then(Value::as_str).unwrap_or("");
        let channel = config
            .get("channel_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let rest = crate::transport::Rest::production(token);
        // 20 s bound, like the Python's `asyncio.wait_for(..., 20)`.
        let send = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            rest.rest_send(channel, content),
        )
        .await;
        match send {
            Ok(Ok(_)) => {
                return json!({"content": "Sent to the configured Discord channel."});
            }
            _ => return is_error(),
        }
    }
    json!({"error": "Unsupported method"})
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
