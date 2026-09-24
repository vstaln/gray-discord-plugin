//! Chat-bound cron: a job added from Discord comes back to the channel it
//! was added from.
//!
//! gray core owns the semantics (schedule, claim, fire, `[SILENT]`, the
//! `Cronjob Response:` frame) and hands the rendered line back through
//! `gray cron tick --json`. This module only carries bytes: it records
//! where a conversation lives (so a job the model adds with a plain
//! `gray cron add` inherits the binding), ticks each conversation's store,
//! and posts what comes back.
//!
//! Deliberately not Hermes' "cron lives in the gateway daemon" layout:
//! every turn here runs in its own gray home, so the jobs live beside the
//! conversation they belong to and are fired with that conversation's
//! credentials, workdir, and skills.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// How often due jobs are claimed. gray's own cron service uses 60s.
pub const TICK_EVERY: Duration = Duration::from_secs(60);

/// Where a conversation's jobs deliver. Written where the channel is known
/// (the gateway, per turn) and read where the home is known (the runner,
/// the ticker) — the binding survives restarts because it is a file.
fn route_path(home: &Path) -> PathBuf {
    home.join("route.json")
}

pub fn write_route(home: &Path, channel: &str, chat: &str) {
    let route = serde_json::json!({
        "platform": "discord",
        "chat": chat,
        "route": channel,
    });
    let _ = crate::config::atomic_json(&route_path(home), &route);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(route_path(home), std::fs::Permissions::from_mode(0o600));
    }
}

pub fn read_route(home: &Path) -> Option<Value> {
    std::fs::read(route_path(home))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// The `GRAY_CRON_ORIGIN` value for a turn: whatever binding this
/// conversation has. `None` outside a chat (a plain local gray home), which
/// keeps `gray cron add` local as before.
pub fn origin_env(home: &Path) -> Option<String> {
    let route = read_route(home)?;
    let platform = route.get("platform")?.as_str()?.trim();
    let chat = route.get("chat")?.as_str()?.trim();
    let route_id = route.get("route")?.as_str()?.trim();
    if platform.is_empty() || chat.is_empty() || route_id.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({
            "platform": platform,
            "chat": chat,
            "route": route_id,
        })
        .to_string(),
    )
}

/// One rendered delivery from `gray cron tick --json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub job_id: String,
    pub channel: String,
    pub text: String,
}

/// Split tick stdout into deliveries and drop everything else. Core emits
/// one `cron_delivery` line per chat-bound fire plus a `cron_tick`
/// summary; a line we cannot parse is ignored rather than delivered —
/// narration must never post garbage into someone's channel.
pub fn parse_tick(stdout: &str) -> Vec<Delivery> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("cron_delivery") {
            continue;
        }
        if v.get("platform").and_then(Value::as_str) != Some("discord") {
            continue;
        }
        let (Some(job_id), Some(text)) = (
            v.get("job_id").and_then(Value::as_str),
            v.get("text").and_then(Value::as_str),
        ) else {
            continue;
        };
        // Prefer the explicit route (the channel); fall back to the chat id
        // for a host that routed by conversation.
        let channel = v
            .get("route")
            .and_then(Value::as_str)
            .or_else(|| v.get("chat").and_then(Value::as_str))
            .unwrap_or("")
            .trim()
            .to_string();
        if channel.is_empty() || text.trim().is_empty() {
            continue;
        }
        out.push(Delivery {
            job_id: job_id.to_string(),
            channel,
            text: text.to_string(),
        });
    }
    out
}

/// Every conversation home that could hold jobs: the route file only
/// exists for a conversation the gateway has actually run.
pub fn routable_homes(conversations_dir: &Path) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let Ok(entries) = std::fs::read_dir(conversations_dir) else {
        return homes;
    };
    for entry in entries.flatten() {
        let home = entry.path();
        if home.join("cron").is_dir() && read_route(&home).is_some() {
            homes.push(home);
        }
    }
    homes.sort();
    homes
}

/// Claim and fire whatever is due in one home. Returns what to post.
/// A tick that fails (gray missing, provider down) is not a turn failure:
/// the next tick tries again, and the store's claim is released.
pub async fn tick_home(gray_bin: &Path, home: &Path) -> Vec<Delivery> {
    let out = tokio::process::Command::new(gray_bin)
        .arg("cron")
        .arg("tick")
        .arg("--json")
        .env("GRAY_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await;
    match out {
        Ok(o) => parse_tick(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    }
}
