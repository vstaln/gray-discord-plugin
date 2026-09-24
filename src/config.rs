//! Port of gray_discord/config.py: private config load/save/validate.
//! Errors never contain config values.
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Default config path: ~/.config/gray-discord/config.json.
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/gray-discord/config.json")
}

/// Write JSON (pretty + trailing newline) atomically: parents 0700,
/// temp file + rename, replacement mode 0600.
pub fn atomic_json(path: &Path, data: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|_| "cannot create config directory".to_string())?;
        }
    }
    let mut text =
        serde_json::to_string_pretty(data).map_err(|_| "cannot encode config".to_string())?;
    text.push('\n');
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, text.as_bytes()).map_err(|_| "cannot write config".to_string())?;
    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "cannot protect config".to_string())?;
    fs::rename(&tmp_path, path).map_err(|_| "cannot replace config".to_string())?;
    Ok(())
}

/// Discord snowflake: ASCII digits, 0 < n < 2^64.
pub fn snowflake(v: &Value) -> bool {
    match v.as_str() {
        Some(s) => {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && s.parse::<u64>().map(|n| n > 0).unwrap_or(false)
        }
        None => false,
    }
}

fn int_in(data: &Value, key: &str, low: u64, high: u64) -> Result<(), String> {
    if let Some(v) = data.get(key) {
        let ok = v.as_u64().is_some_and(|n| (low..=high).contains(&n));
        if !ok {
            return Err(format!("{key} must be an integer between {low} and {high}"));
        }
    }
    Ok(())
}

/// Validate a config object. Error texts match config.py verbatim.
pub fn validate_config(data: &Value) -> Result<(), String> {
    if !data.is_object() {
        return Err("Configuration must be an object".to_string());
    }
    match data.get("token").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => {}
        _ => return Err("Bot token is missing; run setup".to_string()),
    }
    // channel_id is the home channel and always required; owner_id is
    // optional — a token-only config means "nobody admitted yet", and every
    // human DM then gets a pairing reply (the first approval becomes owner).
    if !data.get("channel_id").is_some_and(snowflake) {
        return Err("channel_id must be a Discord ID".to_string());
    }
    if let Some(owner) = data.get("owner_id") {
        if !snowflake(owner) {
            return Err("owner_id must be a Discord ID".to_string());
        }
    }
    if let Some(users) = data.get("allowed_users") {
        let ok = users.as_array().is_some_and(|a| a.iter().all(snowflake));
        if !ok {
            return Err("allowed_users must be an array of Discord IDs".to_string());
        }
    }
    for key in ["gray_bin", "gray_home", "workdir"] {
        let ok = data
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| Path::new(s).is_absolute());
        if !ok {
            return Err(format!("{key} must be an absolute path"));
        }
    }
    int_in(data, "timeout_seconds", 1, 86400)?;
    int_in(data, "concurrency", 1, 16)?;
    int_in(data, "max_requests", 1, 1000)?;
    int_in(data, "queue_capacity", 1, 100000)?;
    if let Some(policy) = data.get("session_reset") {
        if !policy.is_object() {
            return Err("session_reset must be an object".to_string());
        }
        if let Some(mode) = policy.get("mode") {
            let ok = mode
                .as_str()
                .is_some_and(|m| matches!(m, "none" | "idle" | "daily" | "both"));
            if !ok {
                return Err("session_reset.mode must be none, idle, daily, or both".to_string());
            }
        }
        if let Some(minutes) = policy.get("idle_minutes") {
            let ok = minutes.as_u64().is_some_and(|n| (1..=10_080).contains(&n));
            if !ok {
                return Err("session_reset.idle_minutes must be between 1 and 10080".to_string());
            }
        }
        if let Some(hour) = policy.get("at_hour") {
            let ok = hour.as_u64().is_some_and(|n| n <= 23);
            if !ok {
                return Err("session_reset.at_hour must be between 0 and 23".to_string());
            }
        }
    }
    if let Some(policy) = data.get("budget") {
        let model = policy.get("model").and_then(Value::as_str).unwrap_or("");
        crate::budget::validate(policy, model)?;
    }
    Ok(())
}

/// Load + validate. Any IO/parse failure maps to the setup hint.
pub fn load_config(path: &Path) -> Result<Value, String> {
    let data: Value = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| "Configuration missing or invalid; run setup".to_string())?;
    validate_config(&data)?;
    Ok(data)
}

/// Validate + atomic save.
pub fn save_config(path: &Path, data: &Value) -> Result<(), String> {
    validate_config(data)?;
    atomic_json(path, data)
}
