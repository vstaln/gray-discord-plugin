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
    let mut data = json!({"embeds": [embed]});
    if !public {
        data["flags"] = json!(64);
    }
    if !buttons.is_empty() {
        data["components"] = json!([{"type": 1, "components": buttons}]);
    }
    if let Err(e) = ctx
        .rest
        .interaction_callback(ctx.int_id, ctx.int_token, &json!({"type": 4, "data": data}))
        .await
    {
        eprintln!("[discord] command reply failed: {e}");
    }
}

/// Acknowledge first, then post through the webhook — the path for anything
/// that calls out. `public` rides on the deferral, because the followup
/// inherits its visibility from there.
async fn answer_deferred(
    ctx: &Ctx<'_>,
    label: &str,
    public: bool,
    render: impl std::future::Future<Output = Value>,
) {
    // The followup inherits its visibility from this ack, so a private question
    // has to be acked private; it cannot be made private afterwards.
    let mut data = json!({"content": label});
    if !public {
        data["flags"] = json!(64);
    }
    let defer = json!({"type": 5, "data": data});
    if let Err(e) = ctx
        .rest
        .interaction_callback(ctx.int_id, ctx.int_token, &defer)
        .await
    {
        eprintln!("[discord] command defer failed: {e}");
        return;
    }
    let embed = render.await;
    if let Err(e) = ctx
        .rest
        .followup_embed(ctx.app_id, ctx.int_token, &embed)
        .await
    {
        eprintln!("[discord] command followup failed: {e}");
    }
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
            let (embed, buttons) = commands::cron_view(&mine, ctx.channel_id);
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
                answer_deferred(ctx, "reading memory…", true, async move {
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
            let defer = json!({"type": 5});
            if let Err(e) = ctx
                .rest
                .interaction_callback(ctx.int_id, ctx.int_token, &defer)
                .await
            {
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
                    let _ = ctx
                        .rest
                        .followup_embed(
                            ctx.app_id,
                            ctx.int_token,
                            &commands::embed(
                                "/ask",
                                "That turn is already queued (Discord redelivered it).",
                                &[],
                            ),
                        )
                        .await;
                }
                Err(e) => {
                    eprintln!("[discord] /ask enqueue failed: {e}");
                    let _ = ctx
                        .rest
                        .followup_embed(
                            ctx.app_id,
                            ctx.int_token,
                            &commands::embed("/ask", format!("Cannot queue that: {e}"), &[]),
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

/// A pressed button: `cron:remove:<id>`. Only two words of freedom, and the
/// allow-list gate already ran before we got here.
pub async fn button(ctx: &Ctx<'_>, custom_id: &str) {
    if let Some(id) = custom_id.strip_prefix("cron:remove:") {
        match ctx.store.schedule_remove(id) {
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
        }
    }
}
