//! Slash-command handlers.
//!
//! One entry point ([`handle`]), one place that knows how to answer a
//! Discord interaction. Everything here is the adapter around the pure
//! logic in [`crate::commands`]: that module decides what a request *means*,
//! this one executes it and renders the reply.
//!
//! Two reply rules that are easy to get wrong:
//!
//! - Discord accepts exactly **one** initial response per interaction. A
//!   callback that took longer than 3 seconds is dropped, so anything that
//!   shells out acknowledges first and answers through the webhook.
//! - Ephemerality is decided by the *deferral*, not the followup.

use crate::commands::{self, JobRow, Request};
use crate::durable::Store;
use crate::render;
use crate::transport::Rest;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a delegated gray CLI call may take. gray's own memory CLI is
/// file-only and answers in milliseconds; 10s is a guard against a wedged
/// binary, not a budget.
const CLI_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything a handler needs, borrowed so nothing is cloned per reply.
pub struct Ctx<'a> {
    pub user_id: &'a str,
    pub channel_id: &'a str,
    pub is_dm: bool,
    pub int_id: &'a str,
    pub int_token: &'a str,
    pub app_id: &'a str,
    pub rest: &'a Rest,
    pub store: &'a Store,
    pub config: &'a Value,
    pub config_path: &'a Path,
}

impl<'a> Ctx<'a> {
    /// The gray home this channel's turns run in. Same derivation as the
    /// runner's, so a `/memory` call sees exactly what the agent in this
    /// channel sees.
    fn conversation_home(&self) -> PathBuf {
        let base = self
            .config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let home = base
            .join("conversations")
            .join(crate::runner::hex_sha256(self.conversation().as_bytes()));
        ensure_private_dir(&home);
        home
    }

    /// DMs use their channel; guild messages are isolated per user. Discord
    /// supplies a thread's own channel id for messages inside that thread,
    /// so native threads already get separate sessions.
    pub fn conversation(&self) -> String {
        crate::session::conversation_key(self.channel_id, self.user_id, self.is_dm)
    }

    /// The configured home channel, which owns every schedule written before
    /// `/cron` existed.
    fn home_channel(&self) -> &str {
        self.config
            .get("channel_id")
            .and_then(Value::as_str)
            .unwrap_or("")
    }
}

/// Create the conversation home at 0700 where the platform has a mode
/// knob. `DirBuilder::mode` does not exist on Windows, and this crate is
/// built there too.
fn ensure_private_dir(path: &Path) {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let _ = builder.create(path);
}

/// Which gray this channel answers with, per `meta` then gray's own config.
fn current_model(ctx: &Ctx<'_>) -> (Option<String>, Option<String>) {
    let override_model = ctx
        .store
        .meta_get(&commands::model_key(&ctx.conversation()))
        .ok()
        .flatten()
        .filter(|m| !m.trim().is_empty());
    let provider_model = ctx
        .config
        .get("gray_home")
        .and_then(Value::as_str)
        .and_then(|home| std::fs::read(Path::new(home).join("config.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_string))
        .filter(|m| !m.trim().is_empty());
    (override_model, provider_model)
}

/// Run `gray <argv>` in this channel's home and take its text. gray owns the
/// format; the bridge renders what it printed, so there is no second parser
/// to keep in step with it.
async fn gray_cli(ctx: &Ctx<'_>, argv: &[String]) -> Result<(String, String), String> {
    let gray_bin = ctx
        .config
        .get("gray_bin")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if gray_bin.is_empty() {
        return Err("gray executable is missing".to_string());
    }
    let home = ctx.conversation_home();
    let child = tokio::process::Command::new(&gray_bin)
        .args(argv)
        .env("GRAY_HOME", &home)
        .env("GRAY_SHOW_REASONING", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run gray: {e}"))?;
    let out = tokio::time::timeout(CLI_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "gray did not answer in time".to_string())?
        .map_err(|e| format!("gray failed: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// Answer in the same call as the interaction (the 3-second path).
async fn answer(ctx: &Ctx<'_>, embed: &Value, buttons: &[Value], public: bool) {
    let components = match render::from_embed(embed, buttons) {
        Ok(components) => components,
        Err(e) => {
            eprintln!("[discord] invalid command render: {e}");
            vec![json!({"type": 10, "content": "Gray could not render this response."})]
        }
    };
    if let Err(e) = ctx
        .rest
        .interaction_v2(ctx.int_id, ctx.int_token, &components, public)
        .await
    {
        eprintln!("[discord] command reply failed: {e}");
    }
}

async fn edit_deferred(ctx: &Ctx<'_>, embed: &Value, buttons: &[Value], public: bool) {
    let components = match render::from_embed(embed, buttons) {
        Ok(components) => components,
        Err(e) => {
            eprintln!("[discord] invalid deferred render: {e}");
            vec![json!({"type": 10, "content": "Gray could not render this response."})]
        }
    };
    if let Err(e) = ctx
        .rest
        .edit_original_v2(ctx.app_id, ctx.int_token, &components, public)
        .await
    {
        eprintln!("[discord] command original edit failed: {e}");
    }
}

/// Acknowledge first, then edit the original response. `public` is fixed by
/// the deferral because Discord does not let a later edit change visibility.
async fn answer_deferred(
    ctx: &Ctx<'_>,
    public: bool,
    render: impl std::future::Future<Output = Value>,
) {
    if let Err(e) = ctx.rest.defer_v2(ctx.int_id, ctx.int_token, public).await {
        eprintln!("[discord] command defer failed: {e}");
        return;
    }
    let embed = render.await;
    edit_deferred(ctx, &embed, &[], public).await;
}

/// Execute one parsed request. Every arm ends in a reply, so a request that
/// falls through the match would be a silent no-op the user cannot see —
/// which is why `handle` has no other exit.
pub async fn handle(ctx: &Ctx<'_>, request: Request) {
    match request {
        Request::Help { command } => {
            let embed = match command.as_deref() {
                Some(name) => commands::help_for(name).unwrap_or_else(|| {
                    commands::embed(
                        "No such command",
                        format!("`/{name}` is not a command here. `/help` lists them all."),
                        &[],
                    )
                }),
                None => commands::help_embed(),
            };
            answer(ctx, &embed, &[], true).await;
        }
        Request::CronList => {
            let jobs = match ctx.store.schedules() {
                Ok(j) => j,
                Err(e) => return answer(ctx, &commands::embed("Cron", e, &[]), &[], false).await,
            };
            let targets: Vec<String> = jobs.iter().map(|j| j.channel.clone()).collect();
            let rows: Vec<JobRow> = jobs
                .iter()
                .map(|j| (j.id.clone(), j.prompt.clone(), j.interval, j.status.clone()))
                .collect();
            let mine: Vec<JobRow> =
                commands::jobs_for_channel(&rows, &targets, ctx.channel_id, ctx.home_channel())
                    .into_iter()
                    .cloned()
                    .collect();
            let mut state_tokens = Vec::new();
            for job in mine.iter().take(5) {
                match ctx.store.component_state_create(
                    "cron_remove",
                    ctx.user_id,
                    ctx.channel_id,
                    &job.0,
                    86_400,
                ) {
                    Ok(token) => state_tokens.push(token),
                    Err(e) => {
                        eprintln!("[discord] could not create cron button state: {e}");
                        state_tokens.clear();
                        break;
                    }
                }
            }
            let (embed, buttons) = commands::cron_view(&mine, ctx.channel_id, &state_tokens);
            answer(ctx, &embed, &buttons, true).await;
        }
        Request::CronAdd { every, prompt } => {
            let interval = match commands::parse_interval(&every) {
                Some(i) if i >= 60 => i,
                _ => {
                    return answer(
                        ctx,
                        &commands::embed(
                            "Cron",
                            format!(
                            "`{every}` is not an interval. Use `30m`, `2h`, `1d` — 60s or more."
                        ),
                            &[],
                        ),
                        &[],
                        false,
                    )
                    .await
                }
            };
            let id = crate::durable::uuid_hex();
            let conv = ctx.conversation();
            match ctx.store.schedule_add(
                &id,
                interval as u64,
                &prompt,
                ctx.channel_id,
                &conv,
                crate::durable::now_secs(),
            ) {
                Ok(()) => {
                    let embed = commands::embed(
                        "Scheduled",
                        format!(
                            "`{}` will run every {} in <#{}>.\n> {}",
                            id,
                            commands::human(interval),
                            ctx.channel_id,
                            prompt.trim()
                        ),
                        &[],
                    );
                    answer(ctx, &embed, &[], true).await;
                }
                Err(e) => answer(ctx, &commands::embed("Cron", e, &[]), &[], false).await,
            }
        }
        Request::CronRemove { id } => match ctx.store.schedule_remove(&id) {
            Ok(()) => {
                answer(
                    ctx,
                    &commands::embed("Cron", format!("Deleted `{id}`."), &[]),
                    &[],
                    false,
                )
                .await
            }
            Err(e) => answer(ctx, &commands::embed("Cron", e, &[]), &[], false).await,
        },
        Request::ModelShow => {
            let (override_model, provider_model) = current_model(ctx);
            let effective =
                commands::effective_model(override_model.as_deref(), provider_model.as_deref());
            let mut fields = vec![json!({
                "name": "In use here",
                "value": format!("`{effective}`"),
                "inline": false,
            })];
            if let Some(m) = override_model {
                fields.push(json!({
                    "name": "Pinned by /model set",
                    "value": format!("`{m}`"),
                    "inline": false,
                }));
            }
            if let Some(m) = provider_model {
                fields.push(json!({
                    "name": "From gray's config",
                    "value": format!("`{m}`"),
                    "inline": false,
                }));
            }
            answer(
                ctx,
                &commands::embed(
                    "Model",
                    "`/model set <provider/model-id>` changes this channel only.",
                    &fields,
                ),
                &[],
                true,
            )
            .await;
        }
        Request::ModelSet { model } => {
            let model = model.trim().to_string();
            match ctx
                .store
                .meta_set(&commands::model_key(&ctx.conversation()), &model)
            {
                Ok(()) => {
                    answer(
                        ctx,
                        &commands::embed(
                            "Model",
                            format!("This channel now answers with `{model}`."),
                            &[],
                        ),
                        &[],
                        true,
                    )
                    .await
                }
                Err(e) => answer(ctx, &commands::embed("Model", e, &[]), &[], false).await,
            }
        }
        Request::MemoryList
        | Request::MemoryShow { .. }
        | Request::MemorySet { .. }
        | Request::MemoryRemove { .. } => {
            let argv = match &request {
                Request::MemoryList => commands::memory_argv("list", None, None),
                Request::MemoryShow { key } => commands::memory_argv("show", Some(key), None),
                Request::MemorySet { key, text } => {
                    commands::memory_argv("set", Some(key), Some(text))
                }
                Request::MemoryRemove { key } => commands::memory_argv("remove", Some(key), None),
                _ => unreachable!(),
            };
            // Deleting is the destructive one, so it stays with the caller.
            let destructive = matches!(&request, Request::MemoryRemove { .. });
            if destructive {
                let (stdout, stderr) = match gray_cli(ctx, &argv).await {
                    Ok(o) => o,
                    Err(e) => {
                        return answer(ctx, &commands::embed("Memory", e, &[]), &[], false).await
                    }
                };
                answer(
                    ctx,
                    &commands::cli_embed("Memory", &stdout, &stderr, stderr.trim().is_empty()),
                    &[],
                    false,
                )
                .await;
            } else {
                answer_deferred(ctx, true, async move {
                    match gray_cli(ctx, &argv).await {
                        Ok((stdout, stderr)) => commands::cli_embed(
                            "Memory",
                            &stdout,
                            &stderr,
                            stderr.trim().is_empty(),
                        ),
                        Err(e) => commands::embed("Memory", e, &[]),
                    }
                })
                .await;
            }
        }
        Request::Status => {
            let depth = ctx.store.pending_count().unwrap_or(0);
            let (override_model, provider_model) = current_model(ctx);
            let model =
                commands::effective_model(override_model.as_deref(), provider_model.as_deref());
            let embed = commands::embed(
                "Status",
                "Queue and model for this channel.",
                &[
                    json!({"name": "Turns queued", "value": depth.to_string(), "inline": true}),
                    json!({"name": "Model", "value": format!("`{model}`"), "inline": true}),
                    json!({"name": "Bridge", "value": format!("v{}", env!("CARGO_PKG_VERSION")), "inline": true}),
                ],
            );
            answer(ctx, &embed, &[], true).await;
        }
        Request::Ask { prompt } => {
            // The 3-second rule: acknowledge now, enqueue the turn, and let
            // the delivery loop answer through the webhook when gray is done.
            if let Err(e) = ctx.rest.defer_v2(ctx.int_id, ctx.int_token, true).await {
                eprintln!("[discord] /ask defer failed: {e}");
                return;
            }
            let capacity = ctx
                .config
                .get("queue_capacity")
                .and_then(Value::as_u64)
                .unwrap_or(1000);
            let conv = ctx.conversation();
            match ctx
                .store
                .enqueue(ctx.int_id, ctx.channel_id, &prompt, Some(&conv), capacity)
            {
                Ok(true) => {
                    let _ = ctx
                        .store
                        .set_interaction(ctx.int_id, ctx.int_token, ctx.app_id);
                }
                Ok(false) => {
                    edit_deferred(
                        ctx,
                        &commands::embed(
                            "/ask",
                            "That turn is already queued (Discord redelivered it).",
                            &[],
                        ),
                        &[],
                        true,
                    )
                    .await;
                }
                Err(e) => {
                    eprintln!("[discord] /ask enqueue failed: {e}");
                    edit_deferred(
                        ctx,
                        &commands::embed("/ask", format!("Cannot queue that: {e}"), &[]),
                        &[],
                        true,
                    )
                    .await;
                }
            }
        }
        Request::Reset => {
            let _canceled = ctx
                .store
                .cancel_pending_conversation(&ctx.conversation())
                .unwrap_or(false);
            if let Err(e) =
                crate::session::reset_home(&ctx.conversation_home(), crate::durable::now_secs())
            {
                answer(ctx, &commands::embed("Session", e, &[]), &[], false).await;
                return;
            }
            answer(
                ctx,
                &commands::embed("Session", "Session reset.", &[]),
                &[],
                false,
            )
            .await;
        }
        Request::Stop => {
            let stopped = ctx
                .store
                .cancel_conversation(&ctx.conversation())
                .unwrap_or(false);
            let text = if stopped {
                "Stopping the running turn."
            } else {
                "No running turn to stop."
            };
            answer(ctx, &commands::embed("Stop", text, &[]), &[], false).await;
        }
    }
}

/// Acknowledge an interaction shape the current bridge does not understand.
pub async fn unknown(ctx: &Ctx<'_>) {
    answer(
        ctx,
        &commands::embed("Command", "That command is no longer available.", &[]),
        &[],
        false,
    )
    .await;
}

/// Modal submissions are acknowledged even when no modal workflow is
/// currently registered. This prevents Discord's three-second retry storm if
/// an old or manually-created modal reaches the bridge.
pub async fn modal(ctx: &Ctx<'_>, custom_id: &str) {
    eprintln!("[discord] unhandled modal submission: {custom_id}");
    answer(
        ctx,
        &commands::embed("Modal", "This Gray dialog is no longer active.", &[]),
        &[],
        false,
    )
    .await;
}

async fn update_original_or_answer(ctx: &Ctx<'_>, embed: &Value) {
    let components = match render::from_embed(embed, &[]) {
        Ok(components) => components,
        Err(e) => {
            eprintln!("[discord] invalid component update: {e}");
            return;
        }
    };
    if let Err(e) = ctx
        .rest
        .interaction_update_v2(ctx.int_id, ctx.int_token, &components)
        .await
    {
        eprintln!("[discord] component update failed; sending fallback: {e}");
        answer(ctx, embed, &[], false).await;
    }
}

/// The live card's Stop button: `turn:stop:<opaque-token>`. Any admitted
/// user in the turn's channel may press it once. The press flags the turn
/// and answers with the pressed card flipped to "stopping…" at once
/// (UPDATE_MESSAGE); the worker settles it properly on its next frame.
/// `message` is the pressed message's component tree, as Discord sent it.
pub async fn stop_button(ctx: &Ctx<'_>, custom_id: &str, message: Option<&Value>) {
    let token = custom_id.strip_prefix("turn:stop:").unwrap_or("");
    let stopped =
        match ctx
            .store
            .component_state_take(token, ctx.user_id, ctx.channel_id, "turn_stop")
        {
            Ok(Some(turn)) => ctx.store.cancel(&turn).is_ok(),
            Ok(None) => false,
            Err(e) => {
                eprintln!("[discord] stop state failed: {e}");
                false
            }
        };
    if !stopped {
        return expired(ctx, "Stop", "That turn already finished.").await;
    }
    let flipped = message.and_then(|components| crate::stream::stopping(components, custom_id));
    let updated = match flipped {
        Some(components) => ctx
            .rest
            .interaction_update_v2(ctx.int_id, ctx.int_token, &components)
            .await
            .is_ok(),
        None => false,
    };
    if !updated {
        if let Err(e) = ctx.rest.defer_update(ctx.int_id, ctx.int_token).await {
            eprintln!("[discord] stop acknowledgement failed: {e}");
        }
    }
}

/// A settled card's Retry button: `turn:retry:<opaque-token>`. Queues the
/// same prompt again as a new turn in the same conversation; its card
/// appears below, so the press itself changes nothing on screen.
pub async fn retry_button(ctx: &Ctx<'_>, custom_id: &str) {
    let token = custom_id.strip_prefix("turn:retry:").unwrap_or("");
    let turn =
        match ctx
            .store
            .component_state_take(token, ctx.user_id, ctx.channel_id, "turn_retry")
        {
            Ok(Some(turn)) => turn,
            Ok(None) => return expired(ctx, "Retry", "That turn can no longer be retried.").await,
            Err(e) => {
                eprintln!("[discord] retry state failed: {e}");
                return expired(ctx, "Retry", "That turn can no longer be retried.").await;
            }
        };
    let capacity = ctx
        .config
        .get("queue_capacity")
        .and_then(Value::as_u64)
        .unwrap_or(1000);
    let queued = match ctx.store.get(&turn) {
        Ok(Some(item)) if item.input_json.is_none() => ctx
            .store
            .enqueue(
                &format!("{}-retry-{}", item.id, &crate::durable::uuid_hex()[..8]),
                &item.channel,
                &item.prompt,
                Some(&item.conversation),
                capacity,
            )
            .unwrap_or(false),
        _ => false,
    };
    if !queued {
        return expired(ctx, "Retry", "That turn could not be queued again.").await;
    }
    if let Err(e) = ctx.rest.defer_update(ctx.int_id, ctx.int_token).await {
        eprintln!("[discord] retry acknowledgement failed: {e}");
    }
}

/// A settled card's New chat button: `turn:new:<opaque-token>`. Same as
/// `/new`, for the conversation the card belongs to.
pub async fn new_chat_button(ctx: &Ctx<'_>, custom_id: &str) {
    let token = custom_id.strip_prefix("turn:new:").unwrap_or("");
    let conversation =
        match ctx
            .store
            .component_state_take(token, ctx.user_id, ctx.channel_id, "turn_new")
        {
            Ok(Some(conversation)) => conversation,
            _ => return expired(ctx, "New chat", "That button has expired; use /new.").await,
        };
    let _ = ctx.store.cancel_pending_conversation(&conversation);
    let home = ctx
        .config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("conversations")
        .join(crate::runner::hex_sha256(conversation.as_bytes()));
    ensure_private_dir(&home);
    let text = match crate::session::reset_home(&home, crate::durable::now_secs()) {
        Ok(()) => "New chat started. Your next message starts fresh.".to_string(),
        Err(e) => e,
    };
    answer(ctx, &commands::embed("New chat", text, &[]), &[], false).await;
}

/// The private "that button no longer works" reply.
async fn expired(ctx: &Ctx<'_>, title: &str, text: &str) {
    answer(ctx, &commands::embed(title, text, &[]), &[], false).await;
}

/// A pressed button: `cron:remove:<opaque-token>`. The token is
/// consumed atomically and is valid only for the original user/channel.
pub async fn button(ctx: &Ctx<'_>, custom_id: &str) {
    let Some(token) = custom_id.strip_prefix("cron:remove:") else {
        answer(
            ctx,
            &commands::embed("Action", "That action is no longer available.", &[]),
            &[],
            false,
        )
        .await;
        return;
    };
    let resource =
        ctx.store
            .component_state_take(token, ctx.user_id, ctx.channel_id, "cron_remove");
    let id = match resource {
        Ok(Some(id)) => id,
        Ok(None) => {
            answer(
                ctx,
                &commands::embed("Cron", "That action expired; list the jobs again.", &[]),
                &[],
                false,
            )
            .await;
            return;
        }
        Err(e) => {
            eprintln!("[discord] component state failed: {e}");
            answer(
                ctx,
                &commands::embed("Cron", "The action could not be completed.", &[]),
                &[],
                false,
            )
            .await;
            return;
        }
    };
    match ctx.store.schedule_remove(&id) {
        Ok(()) => {
            update_original_or_answer(
                ctx,
                &commands::embed("Cron", format!("Deleted `{id}`."), &[]),
            )
            .await
        }
        Err(e) => answer(ctx, &commands::embed("Cron", e, &[]), &[], false).await,
    }
}
