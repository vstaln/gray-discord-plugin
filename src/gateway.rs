//! Owner-only gateway with durable generation and delivery workers.
//!
//! Port of gray_discord/gateway.py — see implementation plan Task 9.

use crate::durable::{OutboxPart, Store};
use crate::runner::RunError;
use crate::transport::Rest;
use serde_json::Value;
use std::path::{Path, PathBuf};
use twilight_gateway::{EventTypeFlags, Intents, Shard, ShardId, StreamExt};
use twilight_model::gateway::event::Event;

pub fn jobs_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("jobs.json")
}

pub fn open_store(config_path: &Path) -> Result<Store, String> {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let store = Store::new(&parent.join("queue.sqlite"))?;
    let legacy = jobs_path(config_path);
    store.migrate_jobs(&legacy)?;
    Ok(store)
}

pub fn slash_commands_json() -> Value {
    serde_json::json!([
        {
            "name": "ask",
            "description": "Send a prompt to gray",
            "options": [
                {
                    "name": "prompt",
                    "description": "What to ask gray",
                    "type": 3,
                    "required": true
                }
            ]
        },
        {
            "name": "reset",
            "description": "Reset your gray session"
        },
        {
            "name": "status",
            "description": "Show gray session status"
        },
        {
            "name": "stop",
            "description": "Stop the running gray agent"
        }
    ])
}

pub type ReactionHook = std::sync::Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
pub type TypingHook = std::sync::Arc<dyn Fn(u64) + Send + Sync>;
pub type ClockFn = std::sync::Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct Runtime<R, D> {
    pub config: Value,
    pub config_path: PathBuf,
    pub store: Store,
    pub deliver: D,
    pub runner: R,
    pub rest: Option<Rest>,
    pub reaction_hook: Option<ReactionHook>,
    pub typing_hook: Option<TypingHook>,
    pub clock: Option<ClockFn>,
    last_typing: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, f64>>>,
}

impl<R, D> Runtime<R, D> {
    pub fn new(config: Value, config_path: PathBuf, store: Store, deliver: D, runner: R) -> Self {
        Self {
            config,
            config_path,
            store,
            deliver,
            runner,
            rest: None,
            reaction_hook: None,
            typing_hook: None,
            clock: None,
            last_typing: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    pub fn with_rest(mut self, rest: Rest) -> Self {
        self.rest = Some(rest);
        self
    }

    pub fn with_reaction_hook(mut self, hook: ReactionHook) -> Self {
        self.reaction_hook = Some(hook);
        self
    }

    pub fn with_typing_hook(mut self, hook: TypingHook) -> Self {
        self.typing_hook = Some(hook);
        self
    }

    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = Some(clock);
        self
    }

    pub fn now_secs(&self) -> f64 {
        self.clock
            .as_ref()
            .map(|c| c())
            .unwrap_or_else(crate::durable::now_secs)
    }

    pub async fn report_progress(&self, channel: &str) {
        let now = self.now_secs();
        let mut map = self.last_typing.lock().await;
        let last = map.get(channel).copied().unwrap_or(-10.0);
        if now - last >= 8.0 {
            map.insert(channel.to_string(), now);
            drop(map);
            if let Ok(ch) = channel.parse::<u64>() {
                if let Some(ref rest) = self.rest {
                    rest.typing(ch).await;
                }
                if let Some(ref hook) = self.typing_hook {
                    hook(ch);
                }
            }
        }
    }

    pub async fn add_reaction(&self, channel: &str, message_id: &str, emoji: &str) {
        let enabled = self
            .config
            .get("reactions")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            return;
        }
        if let Ok(ch) = channel.parse::<u64>() {
            if let Some(ref rest) = self.rest {
                rest.add_reaction(ch, message_id, emoji).await;
            }
            if let Some(ref hook) = self.reaction_hook {
                hook("add", message_id, emoji);
            }
        }
    }

    pub async fn remove_reaction(&self, channel: &str, message_id: &str, emoji: &str) {
        let enabled = self
            .config
            .get("reactions")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            return;
        }
        if let Ok(ch) = channel.parse::<u64>() {
            if let Some(ref rest) = self.rest {
                rest.remove_reaction(ch, message_id, emoji).await;
            }
            if let Some(ref hook) = self.reaction_hook {
                hook("remove", message_id, emoji);
            }
        }
    }
}

impl<R, D, FutR, FutD> Runtime<R, D>
where
    R: Fn(&Value, &Path, &str, &str) -> FutR,
    FutR: std::future::Future<Output = Result<String, RunError>>,
    D: Fn(OutboxPart) -> FutD,
    FutD: std::future::Future<Output = Result<String, String>>,
{
    pub async fn generate_one(&self) -> Result<bool, String> {
        let item = match self.store.claim()? {
            Some(it) => it,
            None => return Ok(false),
        };

        self.add_reaction(&item.channel, &item.id, "👀").await;
        self.report_progress(&item.channel).await;

        let run_fut = (self.runner)(
            &self.config,
            &self.config_path,
            &item.conversation,
            &item.prompt,
        );
        tokio::pin!(run_fut);

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        ticker.tick().await;

        let result = loop {
            tokio::select! {
                res = &mut run_fut => {
                    break Some(res);
                }
                _ = ticker.tick() => {
                    self.report_progress(&item.channel).await;
                    if let Ok(Some(cur)) = self.store.get(&item.id) {
                        if cur.cancel {
                            self.store.fail(&item.id, "cancelled")?;
                            self.remove_reaction(&item.channel, &item.id, "👀").await;
                            self.add_reaction(&item.channel, &item.id, "❌").await;
                            return Ok(true);
                        }
                    }
                }
            }
        };

        match result {
            Some(Ok(answer)) => {
                let receipt = serde_json::json!({});
                self.store.complete(&item.id, &answer, &receipt)?;
            }
            Some(Err(RunError::Budget(_))) => {
                self.store.fail(&item.id, "budget_blocked")?;
                self.remove_reaction(&item.channel, &item.id, "👀").await;
                self.add_reaction(&item.channel, &item.id, "❌").await;
            }
            Some(Err(RunError::Timeout)) => {
                self.store.fail(&item.id, "timeout")?;
                self.remove_reaction(&item.channel, &item.id, "👀").await;
                self.add_reaction(&item.channel, &item.id, "❌").await;
            }
            Some(Err(_)) => {
                self.store.fail(&item.id, "agent_failed")?;
                self.remove_reaction(&item.channel, &item.id, "👀").await;
                self.add_reaction(&item.channel, &item.id, "❌").await;
            }
            None => {}
        }
        Ok(true)
    }

    pub async fn deliver_one(&self) -> Result<bool, String> {
        let part = match self.store.next_delivery(self.now_secs())? {
            Some(p) => p,
            None => return Ok(false),
        };
        match (self.deliver)(part.clone()).await {
            Ok(msg_id) if !msg_id.trim().is_empty() => {
                self.store.ack(&part.id, part.part, &msg_id)?;
                if let Ok(Some(row)) = self.store.get(&part.id) {
                    if row.state == "sent" {
                        self.remove_reaction(&part.channel, &part.id, "👀").await;
                        self.add_reaction(&part.channel, &part.id, "✅").await;
                    }
                }
            }
            _ => {
                self.store
                    .delivery_failed(&part, "delivery_failed", self.now_secs())?;
            }
        }
        Ok(true)
    }
}

impl<R, D, FutR, FutD> Runtime<R, D>
where
    R: Fn(&Value, &Path, &str, &str) -> FutR + Send + Sync + Clone + 'static,
    FutR: std::future::Future<Output = Result<String, RunError>> + Send + 'static,
    D: Fn(OutboxPart) -> FutD + Send + Sync + Clone + 'static,
    FutD: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    pub async fn run(&self) -> Result<(), String> {
        self.store.recover()?;
        let concurrency = self
            .config
            .get("concurrency")
            .and_then(Value::as_u64)
            .unwrap_or(2) as usize;
        let channel_id = self
            .config
            .get("channel_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let mut set = tokio::task::JoinSet::new();
        let rt = std::sync::Arc::new(self.clone());
        for _ in 0..concurrency {
            let r = rt.clone();
            set.spawn(async move {
                loop {
                    match r.generate_one().await {
                        Ok(true) => {}
                        Ok(false) => {
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        }
                        Err(e) => return Err(e),
                    }
                }
            });
        }
        let r = rt.clone();
        set.spawn(async move {
            loop {
                match r.deliver_one().await {
                    Ok(true) => {}
                    Ok(false) => {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        });
        let r = rt.clone();
        let ch = channel_id.clone();
        set.spawn(async move {
            loop {
                let _ = r.store.enqueue_due(&ch, crate::durable::now_secs());
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });

        if let Some(res) = set.join_next().await {
            set.abort_all();
            match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.to_string()),
            }
        } else {
            Ok(())
        }
    }
}

pub async fn run(config_path: &Path) -> Result<(), String> {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .create(parent)
        .map_err(|_| "cannot create config directory".to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }

    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(parent.join("gateway.lock"))
        .map_err(|_| "cannot open gateway lock".to_string())?;
    use std::os::unix::io::AsRawFd;
    let locked = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return Err("Another gateway is running".to_string());
    }
    let _lock = lock_file;

    let mut config = crate::config::load_config(config_path)?;
    let gray_home = config
        .get("gray_home")
        .and_then(Value::as_str)
        .ok_or_else(|| "gray_home is missing from config".to_string())?;
    let provider_bytes = std::fs::read(Path::new(gray_home).join("config.json"))
        .map_err(|_| "gray provider configuration is missing".to_string())?;
    let provider: Value = serde_json::from_slice(&provider_bytes)
        .map_err(|_| "Invalid gray config.json".to_string())?;
    let model = provider
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "provider model is missing".to_string())?;
    // Budget gates only when a policy exists (gray's setup writes none);
    // `budget set` stays the opt-in accounting path.
    config["budget_required"] = Value::Bool(crate::budget::gate(&config, model)?);

    let store = open_store(config_path)?;

    let token = config
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| "token missing".to_string())?
        .to_string();
    let owner_id = config
        .get("owner_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "owner_id missing".to_string())?
        .to_string();
    let allowed: Vec<String> = config
        .get("allowed_users")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let rest = Rest::new(crate::transport::API_BASE, &token);
    let mut bot_id = rest.login().await.unwrap_or_default();
    let initial_app = rest.application().await.ok().map(|(id, _)| id);

    let intents = Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES | Intents::MESSAGE_CONTENT;
    let mut shard = Shard::new(ShardId::ONE, token.clone(), intents);

    let rest_del = rest.clone();
    let deliver = move |part: OutboxPart| {
        let rest = rest_del.clone();
        Box::pin(async move {
            if let Some(ref token) = part.interaction_token {
                let app_id = part.app_id.as_deref().unwrap_or("");
                let ids = rest
                    .followup(app_id, token, &part.content)
                    .await
                    .map_err(|e| e.to_string())?;
                ids.into_iter()
                    .next()
                    .ok_or_else(|| "Delivery returned no message ID".to_string())
            } else {
                let ch: u64 = part
                    .channel
                    .parse()
                    .map_err(|_| "Invalid channel ID".to_string())?;
                let nonce =
                    crate::runner::hex_sha256(format!("{}:{}", part.id, part.part).as_bytes())
                        [..24]
                        .to_string();
                let id = rest
                    .send(ch, &part.content, Some(&nonce))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(id)
            }
        })
    };

    let runner = |cfg: &Value, pth: &Path, conv: &str, prompt: &str| {
        let cfg = cfg.clone();
        let pth = pth.to_path_buf();
        let conv = conv.to_string();
        let prompt = prompt.to_string();
        Box::pin(async move {
            crate::runner::run_gray(&cfg, &pth, &conv, &prompt, crate::runner::default_opts()).await
        })
    };

    let runtime = Runtime::new(
        config.clone(),
        config_path.to_path_buf(),
        store.clone(),
        deliver,
        runner,
    )
    .with_rest(rest.clone());

    let runtime_task = runtime.run();
    tokio::pin!(runtime_task);

    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    let mut app_id = initial_app;

    let sigterm_recv = async {
        #[cfg(unix)]
        if let Some(ref mut s) = sigterm {
            s.recv().await;
            return;
        }
        #[allow(unreachable_code)]
        std::future::pending::<()>().await
    };
    tokio::pin!(sigterm_recv);

    loop {
        tokio::select! {
            res = &mut runtime_task => {
                return res;
            }
            _ = &mut sigterm_recv => {
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            event = shard.next_event(EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::INTERACTION_CREATE | EventTypeFlags::READY) => {
                match event {
                    Some(Ok(Event::Ready(ready))) => {
                        println!("Discord connected; durable owner-only queue enabled.");
                        let r_app_id = ready.application.id.to_string();
                        bot_id = ready.user.id.to_string();
                        app_id = Some(r_app_id.clone());

                        let cmds = slash_commands_json();
                        let cmds_str = serde_json::to_string(&cmds).unwrap_or_default();
                        let hash = crate::runner::hex_sha256(cmds_str.as_bytes());
                        let cached = store.meta_get("slash_commands_hash").ok().flatten();
                        if cached.as_deref() == Some(&hash) {
                            // Unchanged, skip registration
                        } else if let Err(e) = rest.register_commands(&r_app_id, &cmds).await {
                            eprintln!("[discord] slash command registration failed: {e}");
                        } else {
                            let _ = store.meta_set("slash_commands_hash", &hash);
                        }
                    }
                    Some(Ok(Event::MessageCreate(msg))) => {
                        let m = &msg.0;
                        let author_id = m.author.id.to_string();
                        let channel_id = m.channel_id.to_string();
                        let is_dm = m.guild_id.is_none();
                        let prompt = crate::policy::incoming(
                            &author_id,
                            &owner_id,
                            m.author.bot,
                            is_dm,
                            &m.content,
                            &bot_id,
                            &allowed,
                        );
                        if let Some(prompt) = prompt {
                            let capacity = config
                                .get("queue_capacity")
                                .and_then(Value::as_u64)
                                .unwrap_or(1000);
                            let msg_id = m.id.to_string();
                            let conv = format!("chat:{channel_id}");
                            if store
                                .enqueue(&msg_id, &channel_id, &prompt, Some(&conv), capacity)
                                .is_err()
                            {
                                if let Ok(ch) = channel_id.parse::<u64>() {
                                    let _ = rest
                                        .send(
                                            ch,
                                            "Queue full or message invalid; this message was not accepted.",
                                            None,
                                        )
                                        .await;
                                }
                            }
                        } else if let Some(reply) =
                            crate::pairing::reply_for(&store, &author_id, is_dm, m.author.bot)
                        {
                            // Unknown human DMing the bot: tell them their own
                            // ID and mint a code — the owner approves it with
                            // `gray discord pairing approve discord <code>`.
                            if let Ok(ch) = channel_id.parse::<u64>() {
                                let _ = rest.send(ch, &reply, None).await;
                            }
                        }
                    }
                    Some(Ok(Event::InteractionCreate(ic))) => {
                        let interaction = &ic.0;
                        let user_id = interaction
                            .author_id()
                            .map(|id| id.to_string())
                            .unwrap_or_default();
                        if !crate::policy::is_allowed_user(&user_id, &owner_id, &allowed) {
                            continue;
                        }
                        #[allow(deprecated)]
                        let channel_id = interaction
                            .channel
                            .as_ref()
                            .map(|c| c.id.to_string())
                            .or_else(|| interaction.channel_id.map(|c| c.to_string()))
                            .unwrap_or_default();

                        if let Some(twilight_model::application::interaction::InteractionData::ApplicationCommand(ref cmd)) = interaction.data {
                            let effective_app = app_id
                                .clone()
                                .unwrap_or_else(|| interaction.application_id.to_string());
                            let int_id = interaction.id.to_string();
                            let int_token = &interaction.token;

                            match cmd.name.as_str() {
                                "ask" => {
                                    let prompt = cmd.options.iter().find(|o| o.name == "prompt").and_then(|o| match &o.value {
                                        twilight_model::application::interaction::application_command::CommandOptionValue::String(s) => Some(s.as_str()),
                                        _ => None,
                                    });
                                    if let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
                                        let defer = serde_json::json!({"type": 5});
                                        let _ = rest.interaction_callback(&int_id, int_token, &defer).await;

                                        let capacity = config
                                            .get("queue_capacity")
                                            .and_then(Value::as_u64)
                                            .unwrap_or(1000);
                                        let conv = format!("chat:{channel_id}");
                                        if store.enqueue(&int_id, &channel_id, prompt, Some(&conv), capacity).is_ok() {
                                            let _ = store.set_interaction(&int_id, int_token, &effective_app);
                                        }
                                    }
                                }
                                "reset" => {
                                    for conv in [format!("chat:{channel_id}"), format!("user:{user_id}")] {
                                        let key = crate::runner::hex_sha256(conv.as_bytes());
                                        let base = config_path.parent().unwrap_or_else(|| Path::new("."));
                                        let session_file = base.join("conversations").join(key).join("session.json");
                                        let _ = std::fs::remove_file(session_file);
                                    }
                                    let resp = serde_json::json!({
                                        "type": 4,
                                        "data": {
                                            "content": "Session reset.",
                                            "flags": 64
                                        }
                                    });
                                    let _ = rest.interaction_callback(&int_id, int_token, &resp).await;
                                }
                                "status" => {
                                    let depth = store.pending_count().unwrap_or(0);
                                    let resp = serde_json::json!({
                                        "type": 4,
                                        "data": {
                                            "content": format!("Queue depth: {depth} turn(s) pending/running."),
                                            "flags": 64
                                        }
                                    });
                                    let _ = rest.interaction_callback(&int_id, int_token, &resp).await;
                                }
                                "stop" => {
                                    let conv = format!("chat:{channel_id}");
                                    let stopped = store.cancel_conversation(&conv).unwrap_or(false);
                                    let text = if stopped {
                                        "Stopping running agent turn."
                                    } else {
                                        "No running turn to stop."
                                    };
                                    let resp = serde_json::json!({
                                        "type": 4,
                                        "data": {
                                            "content": text,
                                            "flags": 64
                                        }
                                    });
                                    let _ = rest.interaction_callback(&int_id, int_token, &resp).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Err(e)) => {
                        eprintln!("[discord] gateway error: {e}");
                    }
                    None => {
                        // The stream only ends on a fatal close (bad token,
                        // disabled privileged intent, or dropped network).
                        // Say so: an exit-0 silence would restart-loop under
                        // a supervisor with empty logs.
                        eprintln!(
                            "[discord] gateway closed the connection (token, Message Content Intent, or network); exiting"
                        );
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
