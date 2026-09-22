//! Port of gray_discord/transport.py + hermes-rs discord_tool.rs constants:
//! Discord REST client with safe-mention defaults and bounded sends.
use serde_json::{json, Value};

/// Production Discord API base. Tests inject a loopback stub via `Rest::new`;
/// production call sites always use `Rest::production` (never read a base URL
/// from user config — same rule as the Python's "no api_base" comment).
pub const API_BASE: &str = "https://discord.com/api/v10";
/// Discord message content limit, measured in UTF-16 units (see text.rs).
pub const MAX_CONTENT_LEN: usize = 2000;
/// Hermes discord_tool parity constant (embeds are out of scope in v1).
pub const MAX_EMBEDS: usize = 10;
/// OAuth2 invite permissions: View Channels + Send Messages + Read History.
pub const INVITE_PERMISSIONS: u64 = 1024 + 2048 + 65536;
/// Gateway intent bits for the Task 9 connect (discord.py Intents values).
pub const INTENT_GUILDS: u32 = 1 << 0;
pub const INTENT_GUILD_MESSAGES: u32 = 1 << 9;
pub const INTENT_DIRECT_MESSAGES: u32 = 1 << 12;
pub const INTENT_MESSAGE_CONTENT: u32 = 1 << 15;
/// Application flags read from GET /applications/@me (discord.py
/// ApplicationFlags): full and limited message-content variants.
pub const APP_FLAG_MESSAGE_CONTENT: u64 = 1 << 18;
pub const APP_FLAG_MESSAGE_CONTENT_LIMITED: u64 = 1 << 19;
/// Guild permission bits required in the home channel (doctor check).
pub const PERM_VIEW_CHANNEL: u64 = 1 << 10;
pub const PERM_SEND_MESSAGES: u64 = 1 << 11;
pub const PERM_READ_MESSAGE_HISTORY: u64 = 1 << 16;
pub const HOME_PERMS: u64 = PERM_VIEW_CHANNEL | PERM_SEND_MESSAGES | PERM_READ_MESSAGE_HISTORY;

/// Bot user id (`GET /users/@me` → `id`).
pub type UserId = String;
/// Message id (`POST .../messages` → `id`).
pub type MessageId = String;

/// Channel metadata from `GET /channels/{id}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub id: u64,
    pub kind: u64,
    pub guild_id: Option<String>,
}

impl Channel {
    /// Guild channels always carry `guild_id`; DMs never do. This mirrors the
    /// Python's `isinstance(channel, discord.abc.GuildChannel)` without
    /// hardcoding the channel-type table.
    pub fn is_guild(&self) -> bool {
        self.guild_id.is_some()
    }
}

#[derive(Debug, Clone)]
pub enum TransportError {
    Auth(String),
    Forbidden(String),
    RateLimited(Option<f64>),
    Http(u16, String),
    Net(String),
    Invalid(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(_) => f.write_str("Discord authentication failed; check the bot token"),
            Self::Forbidden(_) => {
                f.write_str("Discord refused the request; check channel permissions")
            }
            Self::RateLimited(_) => f.write_str("Discord rate limit hit; backing off"),
            Self::Http(status, _) => write!(f, "Discord HTTP {status}"),
            Self::Net(_) => f.write_str("Discord request failed; check connectivity"),
            Self::Invalid(msg) => f.write_str(msg),
        }
    }
}
impl std::error::Error for TransportError {}

impl TransportError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited(_) | Self::Net(_) => true,
            Self::Http(s, _) => *s == 429 || *s >= 500,
            _ => false,
        }
    }
}

/// Thin REST client. `base` is a constructor arg so tests can inject the
/// loopback stub; production call sites always pass `API_BASE` (never read a
/// base URL from user config — same rule as the Python's "no api_base").
#[derive(Debug, Clone)]
pub struct Rest {
    base: String,
    client: reqwest::Client,
}

/// Python `len(text)` counts code points, so bounds use `chars().count()` —
/// byte length would wrongly reject emoji-heavy messages under the cap.
fn check_bounds(text: &str, max: usize, msg: &str) -> Result<(), TransportError> {
    if text.trim().is_empty() || text.chars().count() > max {
        return Err(TransportError::Invalid(msg.to_string()));
    }
    Ok(())
}

impl Rest {
    pub fn new(base: &str, token: &str) -> Self {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("Bot {token}")) {
            headers.insert(AUTHORIZATION, v);
        }
        if let Ok(v) = HeaderValue::from_str("application/json") {
            headers.insert(CONTENT_TYPE, v);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("reqwest client builds");
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }

    pub fn production(token: &str) -> Self {
        Self::new(API_BASE, token)
    }

    async fn classify(
        &self,
        status: reqwest::StatusCode,
        resp: reqwest::Response,
    ) -> Result<Value, TransportError> {
        let code = status.as_u16();
        if status.is_success() {
            // 204 No Content (typing) has no body.
            let text = resp.text().await.unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text)
                .map_err(|_| TransportError::Http(code, "bad response".into()));
        }
        if code == 401 || code == 403 {
            // Drain the body so the connection can be reused, then discard it:
            // error bodies may echo request context.
            let _ = resp.bytes().await;
            if code == 401 {
                return Err(TransportError::Auth("unauthorized".into()));
            }
            return Err(TransportError::Forbidden("forbidden".into()));
        }
        if code == 429 {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok());
            let _ = resp.bytes().await;
            return Err(TransportError::RateLimited(retry));
        }
        let _ = resp.bytes().await;
        Err(TransportError::Http(code, "request failed".into()))
    }

    async fn get(&self, path: &str) -> Result<Value, TransportError> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| TransportError::Net(trim_net_error(e)))?;
        let status = resp.status();
        self.classify(status, resp).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, TransportError> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| TransportError::Net(trim_net_error(e)))?;
        let status = resp.status();
        self.classify(status, resp).await
    }

    /// GET /users/@me → bot user id. Proves the token works.
    pub async fn login(&self) -> Result<UserId, TransportError> {
        let v = self.get("/users/@me").await?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| TransportError::Http(200, "bad response".into()))
    }

    /// GET /applications/@me → (app id, message-content-intent flag).
    pub async fn application(&self) -> Result<(String, bool), TransportError> {
        let v = self.get("/applications/@me").await?;
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let flags = v.get("flags").and_then(Value::as_u64).unwrap_or(0);
        let intent =
            flags & APP_FLAG_MESSAGE_CONTENT != 0 || flags & APP_FLAG_MESSAGE_CONTENT_LIMITED != 0;
        Ok((id, intent))
    }

    /// GET /channels/{id} → channel metadata.
    pub async fn fetch_channel(&self, id: u64) -> Result<Channel, TransportError> {
        let v = self.get(&format!("/channels/{id}")).await?;
        let kind = v.get("type").and_then(Value::as_u64).unwrap_or(0);
        let guild_id = v
            .get("guild_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Channel { id, kind, guild_id })
    }

    /// GET /guilds/{guild}/members/{user} → guild-wide permission bits.
    /// The Python resolves channel overwrites via `permissions_for`; doctor
    /// checks the guild-level grant, which is the actionable diagnostic.
    pub async fn guild_member_permissions(
        &self,
        guild_id: &str,
        user_id: &str,
    ) -> Result<u64, TransportError> {
        let v = self
            .get(&format!("/guilds/{guild_id}/members/{user_id}"))
            .await?;
        v.get("permissions")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| TransportError::Http(200, "bad response".into()))
    }

    /// Send text as sequential chunks. Returns the first message id.
    pub async fn send(
        &self,
        channel: u64,
        text: &str,
        nonce: Option<&str>,
    ) -> Result<MessageId, TransportError> {
        check_bounds(text, 20000, "Reply must contain 1–20000 characters")?;
        let chunks =
            crate::text::split_message(text, MAX_CONTENT_LEN).map_err(TransportError::Invalid)?;
        let mut first: Option<String> = None;
        for chunk in &chunks {
            let mut body = json!({"content": chunk, "allowed_mentions": {"parse": []}});
            if let Some(n) = nonce {
                body["nonce"] = json!(n);
            }
            // Deliberately no message_reference: replies never ping.
            let v = self
                .post(&format!("/channels/{channel}/messages"), &body)
                .await?;
            if first.is_none() {
                first = v.get("id").and_then(Value::as_str).map(str::to_string);
            }
        }
        first.ok_or_else(|| TransportError::Http(200, "bad response".into()))
    }

    /// Typing indicator. Best-effort: never fails a turn.
    /// Open (or re-open) the DM channel with a user: POST
    /// /users/@me/channels. Used by setup to make the owner's DM the home
    /// channel without asking them to hunt an ID.
    pub async fn create_dm(&self, user_id: u64) -> Result<u64, TransportError> {
        self.post(
            "/users/@me/channels",
            &serde_json::json!({ "recipient_id": user_id.to_string() }),
        )
        .await
        .and_then(|v| {
            v.get("id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| {
                    TransportError::Invalid("Discord did not return a DM channel".into())
                })
        })
    }

    pub async fn typing(&self, channel: u64) {
        let _ = self
            .post(&format!("/channels/{channel}/typing"), &json!({}))
            .await;
    }

    /// Add/remove reactions. Best-effort: never fail a turn.
    pub async fn add_reaction(&self, channel: u64, message: &str, emoji: &str) {
        let encoded = percent_encode(emoji);
        let url = format!(
            "{}/channels/{channel}/messages/{message}/reactions/{encoded}/@me",
            self.base
        );
        let _ = self.client.put(url).send().await;
    }

    pub async fn remove_reaction(&self, channel: u64, message: &str, emoji: &str) {
        let encoded = percent_encode(emoji);
        let url = format!(
            "{}/channels/{channel}/messages/{message}/reactions/{encoded}/@me",
            self.base
        );
        let _ = self.client.delete(url).send().await;
    }

    /// Bulk-overwrite global slash commands for `app_id`.
    pub async fn register_commands(
        &self,
        app_id: &str,
        cmds: &Value,
    ) -> Result<(), TransportError> {
        let url = format!("{}/applications/{app_id}/commands", self.base);
        let resp = self
            .client
            .put(url)
            .json(cmds)
            .send()
            .await
            .map_err(|e| TransportError::Net(trim_net_error(e)))?;
        let status = resp.status();
        self.classify(status, resp).await.map(|_| ())
    }

    /// Slash interaction ack. The gateway passes the deferred payload
    /// (`{"type": 5}`: DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE).
    pub async fn interaction_callback(
        &self,
        id: &str,
        token: &str,
        data: &Value,
    ) -> Result<(), TransportError> {
        let url = format!("{}/interactions/{id}/{token}/callback", self.base);
        let resp = self
            .client
            .post(url)
            .json(data)
            .send()
            .await
            .map_err(|e| TransportError::Net(trim_net_error(e)))?;
        let status = resp.status();
        self.classify(status, resp).await.map(|_| ())
    }

    /// Slash followup chunks (webhook path). Returns message ids.
    pub async fn followup(
        &self,
        app_id: &str,
        token: &str,
        text: &str,
    ) -> Result<Vec<String>, TransportError> {
        check_bounds(
            text,
            200000,
            "Completed answer must contain 1–200000 characters",
        )?;
        let chunks =
            crate::text::split_message(text, MAX_CONTENT_LEN).map_err(TransportError::Invalid)?;
        let mut ids = Vec::new();
        for chunk in &chunks {
            let body = json!({"content": chunk, "allowed_mentions": {"parse": []}});
            let v = self
                .post(&format!("/webhooks/{app_id}/{token}"), &body)
                .await?;
            ids.push(
                v.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
        }
        Ok(ids)
    }

    /// Port of `rest_send`: login → channel → send. REST only.
    pub async fn rest_send(
        &self,
        channel_id: &str,
        text: &str,
    ) -> Result<MessageId, TransportError> {
        check_bounds(text, 20000, "Content must contain 1–20000 characters")?;
        self.login().await?;
        let id: u64 = channel_id
            .parse()
            .map_err(|_| TransportError::Invalid("channel_id must be a Discord ID".into()))?;
        self.fetch_channel(id).await?;
        self.send(id, text, None).await
    }
}

/// Trim reqwest error display to a category (may contain URLs, never tokens —
/// the token only travels in a header, but URLs still leak channel ids).
fn trim_net_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        return "timeout".to_string();
    }
    if e.is_connect() {
        return "connect failed".to_string();
    }
    "request failed".to_string()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
