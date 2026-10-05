//! Owner-only gateway with durable generation and delivery workers.
//!
//! Port of gray_discord/gateway.py — see implementation plan Task 9.

use crate::component_input::{normalize, normalize_autocomplete, NormalizedInteraction};
use crate::component_media::FileStore;
use crate::component_state::NewEvent;
use crate::durable::{OutboxPart, Store};
use crate::runner::{RunError, RunInput};
use crate::transport::{Rest, TransportError};
use serde_json::Value;
use std::path::{Path, PathBuf};
use twilight_gateway::{EventTypeFlags, Intents, Shard, ShardId, StreamExt};
use twilight_model::application::interaction::InteractionType;
use twilight_model::gateway::event::Event;

struct NormalizedRoute<'a> {
    raw: &'a Value,
    interaction_token: &'a str,
    app_id: &'a str,
    rest: &'a Rest,
    store: &'a Store,
    file_store: Option<&'a FileStore>,
    capacity: u64,
    is_dm: bool,
}

async fn route_normalized_interaction(route: NormalizedRoute<'_>) -> Result<(), String> {
    let NormalizedRoute {
        raw,
        interaction_token,
        app_id,
        rest,
        store,
        file_store,
        capacity,
        is_dm,
    } = route;
    let is_autocomplete = raw.get("type").and_then(Value::as_u64) == Some(4);
    if is_autocomplete {
        let values = normalize_autocomplete(raw).map_err(|error| error.to_string())?;
        let resolved = store
            .resolve_state(&values.state_token, &values.user_id, &values.channel_id)
            .map_err(|error| error.to_string())?;
        let conversation =
            crate::session::conversation_key(&values.channel_id, &values.user_id, is_dm);
        let payload = serde_json::json!({"values": values.values});
        let _receipt = store
            .accept_event(NewEvent {
                interaction_id: values.interaction_id.clone(),
                token: values.state_token.clone(),
                user_id: values.user_id.clone(),
                channel_id: values.channel_id.clone(),
                kind: "autocomplete".to_string(),
                payload,
                conversation,
                interaction_token: Some(interaction_token.to_string()),
                app_id: Some(app_id.to_string()),
                capacity,
            })
            .map_err(|error| error.to_string())?;
        let choices = autocomplete_choices(&values.values, &resolved.state);
        return rest
            .autocomplete_response(&values.interaction_id, interaction_token, &choices)
            .await
            .map_err(|error| error.to_string());
    }

    let mut normalized = normalize(raw).map_err(|error| error.to_string())?;
    let _resolved = store
        .resolve_state(
            &normalized.values().state_token,
            &normalized.values().user_id,
            &normalized.values().channel_id,
        )
        .map_err(|error| error.to_string())?;
    let attachments = normalized.values().attachments.clone();
    let mut managed_files = Vec::new();
    if !attachments.is_empty() {
        let file_store = file_store.ok_or_else(|| "managed media is unavailable".to_string())?;
        for attachment in attachments {
            let file = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                file_store.import_url_authenticated(
                    &normalized.values().user_id,
                    &attachment.url,
                    Some(interaction_token),
                ),
            )
            .await
            .map_err(|_| "managed attachment import timed out".to_string())?
            .map_err(|error| error.to_string())?;
            managed_files.push(file.id);
        }
    } else if !normalized.values().files.is_empty() {
        return Err("managed media is unavailable".to_string());
    }
    normalized.values_mut().files = managed_files;
    let values = normalized.values();
    let mut payload = serde_json::json!({"values": values.values});
    if !values.files.is_empty() {
        payload["files"] = serde_json::json!(values.files);
    }
    let conversation = crate::session::conversation_key(&values.channel_id, &values.user_id, is_dm);
    let _receipt = store
        .accept_event(NewEvent {
            interaction_id: values.interaction_id.clone(),
            token: values.state_token.clone(),
            user_id: values.user_id.clone(),
            channel_id: values.channel_id.clone(),
            kind: normalized.kind().to_string(),
            payload,
            conversation,
            interaction_token: Some(interaction_token.to_string()),
            app_id: Some(app_id.to_string()),
            capacity,
        })
        .map_err(|error| error.to_string())?;
    match normalized {
        NormalizedInteraction::ModalSubmit(_) => rest
            .defer_update(&values.interaction_id, interaction_token)
            .await
            .map_err(|error| error.to_string()),
        NormalizedInteraction::Button(_) | NormalizedInteraction::Select(_) => rest
            .defer_v2(&values.interaction_id, interaction_token, true)
            .await
            .map_err(|error| error.to_string()),
        NormalizedInteraction::Autocomplete(_) => unreachable!("autocomplete handled above"),
    }
}

fn autocomplete_choices(values: &Value, state: &Value) -> Vec<(String, String)> {
    let query = values
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    state
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| {
            let name = option.get("label").and_then(Value::as_str)?;
            let value = option.get("value").and_then(Value::as_str)?;
            if !query.is_empty()
                && !name.to_ascii_lowercase().contains(&query)
                && !value.to_ascii_lowercase().contains(&query)
            {
                return None;
            }
            Some((name.to_string(), value.to_string()))
        })
        .take(25)
        .collect()
}

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

/// Registration JSON for every slash command the bridge serves. Derived
/// from `commands::COMMANDS` so the picker, the dispatcher and `/help`
/// cannot disagree; the hash of this body is what Discord sees, so adding a
/// command re-registers exactly once.
pub fn slash_commands_json() -> Value {
    crate::commands::registration_json()
}

pub type ReactionHook = std::sync::Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
pub type TypingHook = std::sync::Arc<dyn Fn(u64) + Send + Sync>;
pub type ClockFn = std::sync::Arc<dyn Fn() -> f64 + Send + Sync>;
/// Activity hook: (activity text, whether this was an edit of the live bubble).
/// A final card uses the same hook with `false` because it is a new message.
pub type ActivityHook = std::sync::Arc<dyn Fn(&str, bool) + Send + Sync>;

fn activity_key(channel: &str, conversation: &str) -> String {
    format!("{channel}\u{0}{conversation}")
}

/// The `value` of the modal input `custom_id` anywhere in a submitted
/// modal's components (a Label wraps its input one level down).
fn find_value(value: &Value, custom_id: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            if map.get("custom_id").and_then(Value::as_str) == Some(custom_id) {
                if let Some(text) = map.get("value").and_then(Value::as_str) {
                    return Some(text.to_string());
                }
            }
            map.values().find_map(|child| find_value(child, custom_id))
        }
        Value::Array(items) => items.iter().find_map(|child| find_value(child, custom_id)),
        _ => None,
    }
}

/// How a turn's card settled.
#[derive(Debug, Default)]
pub struct Settled {
    /// The answer streamed into the card and is on screen as rendered.
    pub landed: Option<crate::stream::Landed>,
    /// The card whose footer shows how the turn ended, when it is on screen.
    pub status_card: Option<String>,
    /// How many of the offered uploads the landed card shows; the rest still
    /// need delivering.
    pub media_shown: usize,
}

/// A settled card's message id and its components without the buttons.
type RetiredCard = (String, Vec<Value>);

/// The `custom_id`s of a turn card's buttons, minted per turn.
#[derive(Debug, Clone, Default)]
pub struct TurnButtons {
    pub stop: Option<String>,
    pub retry: Option<String>,
    pub new_chat: Option<String>,
}

/// What a failed turn-message request means for the turn (Hermes' edit
/// failure classes): rate limits, 5xx, timeouts and dropped connections are
/// waited out; a refused payload is retried with the next frame; a missing
/// message or a forbidden channel is final.
fn failure(error: &TransportError) -> crate::stream::Failure {
    use crate::stream::Failure;
    match error {
        TransportError::RateLimited(after) => Failure::Busy(after.unwrap_or(1.0)),
        TransportError::Net(_) => Failure::Busy(2.0),
        TransportError::Http(429, _) => Failure::Busy(1.0),
        TransportError::Http(code, _) if *code >= 500 => Failure::Busy(2.0),
        TransportError::Http(404, _) => Failure::Gone,
        TransportError::Auth(_) | TransportError::Forbidden(_) => Failure::Forbidden,
        TransportError::Http(_, _) | TransportError::Invalid(_) => Failure::Refused,
    }
}

/// One line on the daemon's stderr for a turn message that did not land,
/// with Discord's own reason when it gave one. Never message content.
fn log_failure(what: &str, channel: &str, error: &TransportError) {
    let detail = match error {
        TransportError::Http(code, detail) => format!("HTTP {code}: {detail}"),
        TransportError::Net(detail) => format!("network: {detail}"),
        TransportError::RateLimited(after) => format!("rate limited, retry after {after:?}s"),
        TransportError::Invalid(detail) => format!("not sent: {detail}"),
        TransportError::Auth(_) => "unauthorized".to_string(),
        TransportError::Forbidden(_) => "forbidden".to_string(),
    };
    eprintln!("[discord] turn message {what} failed in channel {channel}: {detail}");
}

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
    pub activity_hook: Option<ActivityHook>,
    pub clock: Option<ClockFn>,
    /// Rows the runner has read but nobody has narrated yet.
    pub activity: Option<crate::activity::Sink>,
    last_typing: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, f64>>>,
    /// The live turn per channel/conversation: prose messages and tool
    /// bubbles in channel order (see [`crate::stream`]).
    timelines: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, crate::stream::Timeline>>,
    >,
    /// The last settled card per channel/conversation, as it should look
    /// once the next turn starts (its Retry / New chat buttons gone).
    retired: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, RetiredCard>>>,
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
            activity_hook: None,
            clock: None,
            activity: None,
            last_typing: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            timelines: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            retired: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Wire the runner's progress sink: gray's rows in, the live bubble and
    /// final tool card out. Without it the plugin still runs (typing only).
    pub fn with_activity(mut self, sink: crate::activity::Sink) -> Self {
        self.activity = Some(sink);
        self
    }

    pub fn with_activity_hook(mut self, hook: ActivityHook) -> Self {
        self.activity_hook = Some(hook);
        self
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

    /// The Discord typing indicator, on by default.
    ///
    /// Ported 1:1 from Hermes' platform `typing_indicator` flag: same key
    /// name, same default (on), same gate placement (the adapter refuses
    /// before any typing RPC, so turning it off kills the whole path rather
    /// than one loop of it). Set `"typing_indicator": false` in config.json
    /// and the bot never pokes `/channels/<id>/typing`, so Discord drops the
    /// bubble instead of showing it through most turns — the 8s throttle
    /// below re-pokes it for as long as work is in progress, which is what
    /// reads as "always typing" from the other side.
    pub fn typing_enabled(&self) -> bool {
        self.config
            .get("typing_indicator")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    pub async fn report_progress(&self, channel: &str) {
        if !self.typing_enabled() {
            return;
        }
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

    /// Where a relative `MEDIA:` path resolves: the configured workdir, the
    /// same root the delivery worker uses.
    fn media_cwd(&self) -> PathBuf {
        PathBuf::from(
            self.config
                .get("workdir")
                .and_then(Value::as_str)
                .unwrap_or("."),
        )
    }

    fn media_roots(&self) -> Vec<PathBuf> {
        crate::media_tags::roots_from_config(&self.config)
    }

    /// The gray home for one conversation (the same layout `run_gray`
    /// builds), or `None` when the layout cannot be derived.
    pub fn conversation_home(&self, conversation: &str) -> Option<std::path::PathBuf> {
        let base = self.config_path.parent()?;
        Some(
            base.join("conversations")
                .join(crate::runner::hex_sha256(conversation.as_bytes())),
        )
    }

    /// The live session id for a conversation, when a turn has created one.
    fn session_id(&self, conversation: &str) -> Option<String> {
        let home = self.conversation_home(conversation)?;
        let state = std::fs::read(home.join("session.json")).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&state).ok()?;
        v.get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    /// Narrate whatever the agent just did: tool lines in a live bubble, and
    /// (with streamed prose) the reply itself, all edited in place.
    /// Best-effort throughout; live narration can never fail a turn.
    pub async fn report_activity(&self, channel: &str) {
        self.report_activity_for(channel, "", false).await
    }

    /// Start a fresh card and row history for one turn. Prose streams
    /// only when `text` is set: a slash-command turn answers through its
    /// interaction, so its card carries tool lines only.
    pub async fn begin_activity(&self, channel: &str, conversation: &str) {
        self.begin_turn(
            channel,
            conversation,
            crate::stream::enabled(&self.config),
            TurnButtons::default(),
        )
        .await
    }

    /// `buttons` are the card's Stop, Retry and New chat `custom_id`s, when
    /// they were minted. Only the latest turn offers Retry / New chat: the
    /// previous card in this conversation loses them now.
    pub async fn begin_turn(
        &self,
        channel: &str,
        conversation: &str,
        text: bool,
        buttons: TurnButtons,
    ) {
        if let Some(ref sink) = self.activity {
            crate::activity::begin(sink, conversation);
        }
        let key = activity_key(channel, conversation);
        let previous = self.retired.lock().await.remove(&key);
        if let (Some((id, components)), Some(rest), Ok(ch)) =
            (previous, self.rest.as_ref(), channel.parse::<u64>())
        {
            let _ = rest.edit_v2(ch, &id, &components).await;
        }
        let mut timeline = crate::stream::Timeline::new(text, self.now_secs())
            .with_actions(buttons.retry, buttons.new_chat);
        if let Some(stop) = buttons.stop {
            timeline = timeline.with_stop(stop);
        }
        self.timelines.lock().await.insert(key, timeline);
    }

    /// `force` settles the turn as done: every pending edit goes out
    /// regardless of the edit gap, the cursor comes off, and the timeline is
    /// retired.
    pub async fn report_activity_at(&self, channel: &str, force: bool) {
        self.report_activity_for(channel, "", force).await
    }

    /// Activity rows are scoped by conversation, not merely channel. A
    /// daemon can run several channels concurrently; one global queue would
    /// let a fast turn drain or finalize another turn's terminal output.
    pub async fn report_activity_for(&self, channel: &str, conversation: &str, force: bool) {
        if force {
            self.finish_turn(
                channel,
                conversation,
                None,
                &[],
                crate::stream::Status::Done,
            )
            .await;
        } else {
            self.absorb_rows(channel, conversation).await;
            self.publish_timeline(channel, conversation, false, &[])
                .await;
        }
    }

    /// Keep one turn's messages current until `stop` is signalled: gray's
    /// rows in, sends and edits out, about four frames a second. Runs beside
    /// the gray child rather than between reads of its output (Hermes'
    /// stream consumer is its own task for the same reason): a slow or
    /// rate-limited Discord request must never stall the agent or the rows
    /// queued behind it. Returns after the request in flight has finished,
    /// so a post is never cut off halfway and sent twice.
    pub async fn drive_turn(&self, channel: &str, conversation: &str, stop: &tokio::sync::Notify) {
        let mut every = tokio::time::interval(std::time::Duration::from_millis(250));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = stop.notified() => return,
                _ = every.tick() => {}
            }
            self.report_progress(channel).await;
            self.report_activity_for(channel, conversation, false).await;
        }
    }

    /// Settle the turn's card with `status`. With `answer` (prose, `MEDIA:`
    /// tags already taken out) the streamed reply becomes that answer in
    /// place, with `uploads` (the files those tags named) shown inside the
    /// card; `landed` says where it landed, or is `None` when the caller
    /// must post it durably.
    pub async fn finish_turn(
        &self,
        channel: &str,
        conversation: &str,
        answer: Option<&str>,
        uploads: &[crate::media_tags::Upload],
        status: crate::stream::Status,
    ) -> Settled {
        self.absorb_rows(channel, conversation).await;
        let key = activity_key(channel, conversation);
        let now = self.now_secs();
        let shown = uploads.len().min(crate::stream::MAX_MEDIA);
        let adopted = {
            let mut timelines = self.timelines.lock().await;
            match timelines.get_mut(&key) {
                Some(timeline) => {
                    let adopted = answer.is_some_and(|answer| timeline.adopt(answer));
                    if adopted {
                        timeline.attach(
                            uploads[..shown]
                                .iter()
                                .map(|upload| crate::stream::Media {
                                    name: upload.name.clone(),
                                    visual: upload.is_visual(),
                                })
                                .collect(),
                        );
                    }
                    timeline.settle(status, now);
                    adopted
                }
                None => false,
            }
        };
        // A busy Discord (rate limit, 5xx, timeout) gets a couple more
        // chances before the answer falls back to a durable post.
        for _ in 0..3 {
            let Some(wait) = self
                .publish_timeline(channel, conversation, true, uploads)
                .await
            else {
                break;
            };
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait.clamp(0.5, 5.0))).await;
        }
        let timeline = self.timelines.lock().await.remove(&key);
        if let Some(retired) = timeline.as_ref().and_then(crate::stream::Timeline::retired) {
            self.retired.lock().await.insert(key.clone(), retired);
        }
        if let Some(ref sink) = self.activity {
            let history = crate::activity::finish(sink, conversation);
            if crate::activity::enabled(&self.config) && crate::activity::card_enabled(&self.config)
            {
                if let Some(card) = crate::activity::render_card(&history) {
                    self.publish_activity_card(channel, &card).await;
                }
            }
        }
        let Some(timeline) = timeline else {
            return Settled::default();
        };
        let mut settled = Settled {
            landed: None,
            status_card: timeline.status_card(now),
            media_shown: 0,
        };
        if !adopted {
            return settled;
        }
        settled.landed = timeline.landed(now);
        if settled.landed.is_some() {
            settled.media_shown = shown;
        }
        if settled.landed.is_none() {
            // The answer did not fully land. Retract its preview so the
            // durable resend does not leave the same words on screen twice;
            // the tool lines above it stay as the turn's record.
            eprintln!("[discord] the answer did not land in place in channel {channel}; posting it durably");
            if let (Some(rest), Ok(ch)) = (self.rest.as_ref(), channel.parse::<u64>()) {
                for id in timeline.answer_ids() {
                    let _ = rest.delete_message(ch, &id).await;
                }
            }
            settled.status_card = None;
        }
        settled
    }

    /// Move the rows the runner has read into this turn's timeline.
    async fn absorb_rows(&self, channel: &str, conversation: &str) {
        let Some(sink) = self.activity.clone() else {
            return;
        };
        let rows = crate::activity::drain_for(&sink, conversation);
        let narrate = crate::activity::enabled(&self.config);
        let key = activity_key(channel, conversation);
        let now = self.now_secs();
        let mut timelines = self.timelines.lock().await;
        let timeline = timelines
            .entry(key)
            .or_insert_with(|| crate::stream::Timeline::new(false, now));
        for row in rows {
            // Narration off hides tool lines, never the reply.
            if !narrate && row.get("phase").and_then(Value::as_str) != Some("text") {
                continue;
            }
            timeline.absorb(&row);
        }
    }

    /// Send whatever the timeline says is due, in order. A message that
    /// shows files carries the matching `uploads` with its request. Returns
    /// how long to wait when Discord was busy and the frame stopped early.
    async fn publish_timeline(
        &self,
        channel: &str,
        conversation: &str,
        finishing: bool,
        uploads: &[crate::media_tags::Upload],
    ) -> Option<f64> {
        let files_for = |body: &crate::stream::Card| -> Vec<crate::media_tags::Upload> {
            body.files
                .iter()
                .filter_map(|name| uploads.iter().find(|upload| &upload.name == name))
                .cloned()
                .collect()
        };
        let Ok(ch) = channel.parse::<u64>() else {
            return None;
        };
        let key = activity_key(channel, conversation);
        let now = self.now_secs();
        let ops = match self.timelines.lock().await.get(&key) {
            Some(timeline) => timeline.plan(now, crate::activity::MIN_EDIT_GAP, finishing),
            None => return None,
        };
        let mut busy: Option<f64> = None;
        for op in ops {
            match op {
                crate::stream::Op::Post { card, body } => {
                    let files = files_for(&body);
                    let sent = match self.rest {
                        Some(ref rest) if files.is_empty() => {
                            rest.send_v2(ch, &body.components, None).await
                        }
                        Some(ref rest) => {
                            rest.send_v2_uploads(ch, &body.components, &files, None)
                                .await
                        }
                        // No REST (tests, dry runs): still narrate via the hook.
                        None => Ok(format!("hook-{card}")),
                    };
                    let at = self.now_secs();
                    let mut timelines = self.timelines.lock().await;
                    let timeline = timelines.get_mut(&key)?;
                    match sent {
                        Ok(id) => timeline.posted(card, &id, &body, at),
                        Err(error) => {
                            // Messages keep channel order, so nothing after
                            // this one goes out this frame.
                            log_failure("post", channel, &error);
                            let failure = failure(&error);
                            timeline.post_failed(failure, at);
                            if let crate::stream::Failure::Busy(wait) = failure {
                                return Some(wait);
                            }
                            return None;
                        }
                    }
                    drop(timelines);
                    // Discord drops the typing bubble when the bot posts;
                    // the next frame pokes it again (Hermes restores it the
                    // same way after each progress message).
                    if !finishing {
                        self.last_typing.lock().await.remove(channel);
                    }
                    if let Some(ref hook) = self.activity_hook {
                        hook(&body.text, false);
                    }
                }
                crate::stream::Op::Edit { card, id, body } => {
                    let files = files_for(&body);
                    let outcome = match self.rest {
                        Some(ref rest) if files.is_empty() => {
                            rest.edit_v2(ch, &id, &body.components).await
                        }
                        Some(ref rest) => {
                            rest.edit_v2_uploads(ch, &id, &body.components, &files)
                                .await
                        }
                        None => Ok(()),
                    };
                    let at = self.now_secs();
                    let mut timelines = self.timelines.lock().await;
                    let timeline = timelines.get_mut(&key)?;
                    match outcome {
                        Ok(()) => timeline.edited(card, &body, at),
                        // Only this message is affected: the turn goes on,
                        // and its next step is a new message anyway.
                        Err(error) => {
                            log_failure("edit", channel, &error);
                            let failure = failure(&error);
                            timeline.edit_failed(card, failure, at);
                            if let crate::stream::Failure::Busy(wait) = failure {
                                busy = Some(busy.map_or(wait, |seen: f64| seen.max(wait)));
                            }
                            continue;
                        }
                    }
                    drop(timelines);
                    if let Some(ref hook) = self.activity_hook {
                        hook(&body.text, true);
                    }
                }
                crate::stream::Op::Delete { card, id } => {
                    if let Some(ref rest) = self.rest {
                        if let Err(error) = rest.delete_message(ch, &id).await {
                            log_failure("delete", channel, &error);
                        }
                    }
                    if let Some(timeline) = self.timelines.lock().await.get_mut(&key) {
                        timeline.deleted(card);
                    }
                }
            }
        }
        busy
    }

    async fn publish_activity_card(&self, channel: &str, text: &str) {
        let Ok(ch) = channel.parse::<u64>() else {
            return;
        };
        let text = crate::text::sanitize(text);
        let components = match crate::render::tool_card(&text) {
            Ok(components) => components,
            Err(_) => return,
        };
        if let Some(ref rest) = self.rest {
            if rest.send_v2(ch, &components, None).await.is_err() {
                return;
            }
        }
        if let Some(ref hook) = self.activity_hook {
            hook(&text, false);
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
    R: Fn(&Value, &Path, &str, &RunInput) -> FutR,
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
        // A new turn gets a new timeline; the prior turn's messages stay in
        // the channel history instead of being edited underneath. A slash
        // command's answer belongs to its interaction, so only ordinary
        // turns stream their prose into the channel.
        let stream_text = crate::stream::enabled(&self.config) && item.interaction_token.is_none();
        // The card's Stop button: an opaque token bound to this channel and
        // turn, good for as long as the turn may run.
        let ttl = self
            .config
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(600)
            .saturating_add(120);
        let mint = |kind: &str, resource: &str, ttl: u64| {
            self.store
                .component_state_create(&format!("turn_{kind}"), "*", &item.channel, resource, ttl)
                .ok()
                .map(|token| format!("turn:{kind}:{token}"))
        };
        // Retry and New chat outlive the turn by a day. A slash command's
        // card is not the answer, and a component event cannot be replayed
        // as text, so those get Stop only.
        let ordinary = item.interaction_token.is_none();
        let buttons = TurnButtons {
            stop: mint("stop", &item.id, ttl),
            retry: (ordinary && item.input_json.is_none())
                .then(|| mint("retry", &item.id, 86_400))
                .flatten(),
            new_chat: ordinary
                .then(|| mint("new", &item.conversation, 86_400))
                .flatten(),
        };
        self.begin_turn(&item.channel, &item.conversation, stream_text, buttons)
            .await;
        // Bind this conversation's cron jobs to this channel. The chat id
        // is the live session when we have one (so a cron reply continues
        // in context), else the conversation key.
        if let Some(home) = self.conversation_home(&item.conversation) {
            let chat = self
                .session_id(&item.conversation)
                .unwrap_or_else(|| item.conversation.clone());
            crate::cron::write_route(&home, &item.channel, &chat);
        }

        let input = match item.input_json.as_deref() {
            Some(encoded) => RunInput::Structured(
                serde_json::from_str(encoded)
                    .map_err(|_| "queued structured input is invalid".to_string())?,
            ),
            None => RunInput::Text(item.prompt.clone()),
        };
        let mut run_fut = Box::pin((self.runner)(
            &self.config,
            &self.config_path,
            &item.conversation,
            &input,
        ));

        // The turn's messages are driven beside the run, never between its
        // reads: a slow Discord request must not stall gray's output.
        let stop_messages = tokio::sync::Notify::new();
        let messages = self.drive_turn(&item.channel, &item.conversation, &stop_messages);
        tokio::pin!(messages);
        let mut messages_done = false;

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        ticker.tick().await;

        let result = loop {
            tokio::select! {
                res = &mut run_fut => {
                    break Some(res);
                }
                _ = &mut messages, if !messages_done => {
                    messages_done = true;
                }
                _ = ticker.tick() => {
                    if let Ok(Some(cur)) = self.store.get(&item.id) {
                        if cur.cancel {
                            break None;
                        }
                    }
                }
            }
        };
        // Let the request in flight finish (bounded by the REST timeouts),
        // then settle the turn from here.
        stop_messages.notify_one();
        if !messages_done {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(45), &mut messages).await;
        }
        let Some(result) = result else {
            // Stopped: end the gray child (and its process group) first.
            drop(run_fut);
            let settled = self
                .finish_turn(
                    &item.channel,
                    &item.conversation,
                    None,
                    &[],
                    crate::stream::Status::Stopped,
                )
                .await;
            self.fail_turn(&item.id, "cancelled", &settled)?;
            self.remove_reaction(&item.channel, &item.id, "👀").await;
            self.add_reaction(&item.channel, &item.id, "❌").await;
            return Ok(true);
        };

        // Settle the turn. The answer is its last message: the streamed
        // prose becomes it in place (Hermes' final edit), or it is posted
        // below the tool lines, with the files its `MEDIA:` tags named shown
        // inside it; any it cannot hold go through the outbox.
        let (prose, media) = match &result {
            Ok(answer) if stream_text => {
                let (prose, media) =
                    crate::media_tags::extract(answer, &self.media_cwd(), &self.media_roots());
                (Some(prose), media)
            }
            _ => (None, Vec::new()),
        };
        let status = match &result {
            Ok(_) => crate::stream::Status::Done,
            Err(RunError::Budget(_)) => crate::stream::Status::Failed("hit the budget".to_string()),
            Err(RunError::Timeout) => crate::stream::Status::Failed("timed out".to_string()),
            Err(RunError::RequestLimit(n)) => {
                crate::stream::Status::Failed(format!("hit the {n}-request limit"))
            }
            Err(_) => crate::stream::Status::Failed("failed".to_string()),
        };
        let loaded = crate::media_tags::load_pairs(&media);
        let uploads: Vec<crate::media_tags::Upload> =
            loaded.iter().map(|(_, upload)| upload.clone()).collect();
        let settled = self
            .finish_turn(
                &item.channel,
                &item.conversation,
                prose.as_deref(),
                &uploads,
                status,
            )
            .await;
        match result {
            Ok(answer) => {
                let receipt = serde_json::json!({});
                let leftover: Vec<String> = loaded
                    .iter()
                    .skip(settled.media_shown)
                    .map(|(path, _)| format!("MEDIA:{}", path.display()))
                    .collect();
                match settled.landed {
                    Some(landed) if !landed.parts.is_empty() || !leftover.is_empty() => {
                        let media = (!leftover.is_empty()).then(|| leftover.join("\n"));
                        self.store.complete_streamed(
                            &item.id,
                            &receipt,
                            &landed.parts,
                            media.as_deref(),
                        )?;
                        if media.is_none() {
                            self.remove_reaction(&item.channel, &item.id, "👀").await;
                            self.add_reaction(&item.channel, &item.id, "✅").await;
                        }
                    }
                    _ => self.store.complete(&item.id, &answer, &receipt)?,
                }
            }
            Err(RunError::Budget(_)) => {
                self.fail_turn(&item.id, "budget_blocked", &settled)?;
                self.remove_reaction(&item.channel, &item.id, "👀").await;
                self.add_reaction(&item.channel, &item.id, "❌").await;
            }
            Err(RunError::Timeout) => {
                self.fail_turn(&item.id, "timeout", &settled)?;
                self.remove_reaction(&item.channel, &item.id, "👀").await;
                self.add_reaction(&item.channel, &item.id, "❌").await;
            }
            Err(_) => {
                self.fail_turn(&item.id, "agent_failed", &settled)?;
                self.remove_reaction(&item.channel, &item.id, "👀").await;
                self.add_reaction(&item.channel, &item.id, "❌").await;
            }
        }
        Ok(true)
    }

    /// Record a failed turn. When its card's footer already says how it
    /// ended, that card is the notice; otherwise the notice is posted.
    fn fail_turn(&self, id: &str, code: &str, settled: &Settled) -> Result<(), String> {
        match settled.status_card.as_deref() {
            Some(card) => self.store.fail_shown(id, code, card),
            None => self.store.fail(id, code),
        }
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

/// Questions a plugin asks mid-turn (gray's `host/ask`), shown in the
/// channel the turn came from. The channel is the route written just before
/// the turn starts; a conversation without one (nothing to show it in)
/// leaves questions unanswered, as in any headless run.
fn ask_handler(
    rest: &Rest,
    store: &Store,
    config_path: &Path,
    conversation: &str,
) -> Option<crate::runner::AskFn> {
    let home = config_path
        .parent()?
        .join("conversations")
        .join(crate::runner::hex_sha256(conversation.as_bytes()));
    let rest = rest.clone();
    let store = store.clone();
    Some(std::sync::Arc::new(move |questions: Value, turn_over| {
        let rest = rest.clone();
        let store = store.clone();
        let home = home.clone();
        Box::pin(async move {
            let channel = crate::cron::read_route(&home)
                .and_then(|route| route.get("route")?.as_str()?.parse::<u64>().ok());
            let questions = crate::ask::from_host(&questions);
            match channel {
                Some(channel) if !questions.is_empty() => {
                    crate::ask::ask(
                        &rest,
                        &store,
                        channel,
                        &questions,
                        std::time::Duration::from_secs(crate::ask::ASK_SECS),
                        std::time::Duration::from_millis(500),
                        &turn_over,
                    )
                    .await
                }
                _ => serde_json::json!({}),
            }
        })
    }))
}

impl<R, D, FutR, FutD> Runtime<R, D>
where
    R: Fn(&Value, &Path, &str, &RunInput) -> FutR + Send + Sync + Clone + 'static,
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

    // A pidfile next to the config: the /gateway row reads it to tell a
    // connected daemon from a stale state file. Removed on every exit path
    // (drop guard), so a crashed run never leaves a green light behind.
    let pid_path = parent.join("daemon.json");
    {
        let stamp = serde_json::json!({
            "pid": std::process::id(),
            "started_at": crate::durable::now_secs()
        });
        let _ = crate::config::atomic_json(&pid_path, &stamp);
    }
    struct RemovePid(std::path::PathBuf);
    impl Drop for RemovePid {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _remove_pid = RemovePid(pid_path);

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
    let media_root = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("media");
    let conversation_root = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("conversations");
    let file_store = FileStore::new(store.clone(), media_root, conversation_root).ok();

    let token = config
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| "token missing".to_string())?
        .to_string();
    // Ownerless is a legal bootstrap state (OpenClaw parity): nobody is
    // admitted, and every human DM draws a pairing reply until the operator
    // approves a code. An empty owner admits nobody — never someone else.
    let owner_id = config
        .get("owner_id")
        .and_then(Value::as_str)
        .filter(|o| !o.is_empty())
        .unwrap_or_default()
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

    // Sliding-window burst guard per user, from grayai_legacy's rate_limiter:
    // without it one eager DMer is twenty concurrent gray processes.
    let limiter = std::sync::Arc::new(std::sync::Mutex::new(crate::ratelimit::RateLimiter::new(
        config
            .get("rate_limit_capacity")
            .and_then(Value::as_u64)
            .unwrap_or(8) as usize,
        config
            .get("rate_limit_window_secs")
            .and_then(Value::as_u64)
            .unwrap_or(60),
    )));
    let workdir = config
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string();

    let rest = Rest::new(crate::transport::API_BASE, &token);
    let mut bot_id = rest.login().await.unwrap_or_default();
    let initial_app = rest.application().await.ok().map(|(id, _)| id);

    let intents = Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES | Intents::MESSAGE_CONTENT;
    let mut shard = Shard::new(ShardId::ONE, token.clone(), intents);

    let rest_del = rest.clone();
    let media_root = PathBuf::from(&workdir);
    let media_roots = crate::media_tags::roots_from_config(&config);
    let deliver = move |part: OutboxPart| {
        let rest = rest_del.clone();
        let media_root = media_root.clone();
        let media_roots = media_roots.clone();
        Box::pin(async move {
            // Hermes-style `MEDIA:<path>` tags: strip them from the prose and
            // upload the files as a V2 gallery/file message after the text.
            let (prose, media) = if part.document_json.is_none() {
                crate::media_tags::extract(&part.content, &media_root, &media_roots)
            } else {
                (part.content.clone(), Vec::new())
            };
            if !media.is_empty() {
                let text = crate::text::sanitize(&prose);
                let mut first = None;
                if !text.trim().is_empty() {
                    let mut text_part = part.clone();
                    text_part.content = prose.clone();
                    first = Some(deliver_text(&rest, text_part).await?);
                }
                let ch: u64 = part
                    .channel
                    .parse()
                    .map_err(|_| "Invalid channel ID".to_string())?;
                let uploads = crate::media_tags::load(&media);
                for (index, batch) in uploads
                    .chunks(crate::media_tags::MAX_UPLOADS_PER_MESSAGE)
                    .enumerate()
                {
                    let components = crate::media_tags::components(batch, None);
                    let nonce = crate::runner::hex_sha256(
                        format!("{}:{}:media:{index}", part.id, part.part).as_bytes(),
                    )[..24]
                        .to_string();
                    let id = rest
                        .send_v2_uploads(ch, &components, batch, Some(&nonce))
                        .await
                        .map_err(|e| e.to_string())?;
                    first.get_or_insert(id);
                }
                return first.ok_or_else(|| "Delivery returned no message ID".to_string());
            }
            deliver_text(&rest, part).await
        })
            as std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
    };

    async fn deliver_text(rest: &Rest, part: OutboxPart) -> Result<String, String> {
        let text = crate::text::sanitize(&part.content);
        let v2 = part.render.as_deref() == Some("v2");
        if let Some(document_json) = part.document_json.as_deref() {
            let Ok(document) = serde_json::from_str::<Value>(document_json) else {
                return Err("stored component document is invalid".to_string());
            };
            if document
                .get("components")
                .and_then(Value::as_array)
                .is_none()
            {
                return Err("stored component document has no components".to_string());
            }
            if let Some(ref token) = part.interaction_token {
                let app_id = part.app_id.as_deref().unwrap_or("");
                return rest
                    .edit_original_stored_document(app_id, token, &document)
                    .await
                    .map_err(|error| error.to_string());
            }
            let ch: u64 = part
                .channel
                .parse()
                .map_err(|_| "Invalid channel ID".to_string())?;
            return rest
                .send_stored_document(ch, &document, None)
                .await
                .map_err(|error| error.to_string());
        }
        if let Some(ref token) = part.interaction_token {
            let app_id = part.app_id.as_deref().unwrap_or("");
            if v2 {
                if part.part == 0 {
                    let components =
                        crate::render::text_message(&text).map_err(|e| e.to_string())?;
                    rest.edit_original_v2(app_id, token, &components, true)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    let ids = rest
                        .followup_v2(app_id, token, &text)
                        .await
                        .map_err(|e| e.to_string())?;
                    ids.into_iter()
                        .next()
                        .ok_or_else(|| "Delivery returned no message ID".to_string())
                }
            } else {
                let ids = rest
                    .followup(app_id, token, &text)
                    .await
                    .map_err(|e| e.to_string())?;
                ids.into_iter()
                    .next()
                    .ok_or_else(|| "Delivery returned no message ID".to_string())
            }
        } else {
            let ch: u64 = part
                .channel
                .parse()
                .map_err(|_| "Invalid channel ID".to_string())?;
            let nonce = crate::runner::hex_sha256(format!("{}:{}", part.id, part.part).as_bytes())
                [..24]
                .to_string();
            if v2 {
                rest.send_text_v2(ch, &text, Some(&nonce))
                    .await
                    .map_err(|e| e.to_string())
            } else {
                rest.send(ch, &text, Some(&nonce))
                    .await
                    .map_err(|e| e.to_string())
            }
        }
    }

    // One narration sink per daemon: the runner pushes gray's progress
    // rows, the Runtime drains them into the channel's status bubble.
    let activity = crate::activity::sink();
    let runner_sink = activity.clone();
    let ask_rest = rest.clone();
    let ask_store = store.clone();
    let runner = move |cfg: &Value, pth: &Path, conv: &str, input: &RunInput| {
        let cfg = cfg.clone();
        let pth = pth.to_path_buf();
        let conv = conv.to_string();
        let input = input.clone();
        let sink = runner_sink.clone();
        let ask = ask_handler(&ask_rest, &ask_store, &pth, &conv);
        Box::pin(async move {
            let opts = crate::runner::RunOpts {
                progress: Some(crate::activity::callback_for(sink, conv.clone())),
                ask,
                ..crate::runner::default_opts()
            };
            crate::runner::run_gray_input(&cfg, &pth, &conv, input, opts).await
        })
    };

    let runtime = Runtime::new(
        config.clone(),
        config_path.to_path_buf(),
        store.clone(),
        deliver,
        runner,
    )
    .with_rest(rest.clone())
    .with_activity(activity);

    let runtime_task = runtime.run();
    tokio::pin!(runtime_task);

    // Chat-bound cron. Its own task: firing a job spawns a gray turn, and
    // that must never stall shard events (a typing indicator that stops
    // updating mid-turn is a broken gateway).
    let cron_rest = rest.clone();
    let cron_bin = std::path::PathBuf::from(
        config
            .get("gray_bin")
            .and_then(Value::as_str)
            .unwrap_or("gray"),
    );
    let cron_conversations = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("conversations");
    let cron_task = async move {
        let mut every = tokio::time::interval(crate::cron::TICK_EVERY);
        // A slow tick must not burst-fire the backlog.
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            for home in crate::cron::routable_homes(&cron_conversations) {
                for delivery in crate::cron::tick_home(&cron_bin, &home).await {
                    let Ok(ch) = delivery.channel.parse::<u64>() else {
                        continue;
                    };
                    let _ = crate::cron::post_delivery(&cron_rest, ch, &delivery).await;
                }
            }
        }
    };
    tokio::pin!(cron_task);

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
            _ = &mut cron_task => {
                // The cron task never returns; if it does, the daemon is
                // broken rather than idle.
                return Err("cron task stopped".to_string());
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
                        if owner_id.is_empty() {
                            println!("Discord connected; nobody admitted yet (pairing replies only).");
                        } else {
                            println!("Discord connected; durable owner-only queue enabled.");
                        }
                        {
                            // Pin the identity for gray's /gateway row: files
                            // only, no socket, no token. Written after the
                            // gateway is up so a half-connected daemon leaves
                            // the previous value in place.
                            let state_path = config_path
                                .parent()
                                .unwrap_or_else(|| Path::new("."))
                                .join("state.json");
                            let bot = format!(
                                "{}#{}",
                                ready.user.name, ready.user.discriminator
                            );
                            let state = serde_json::json!({
                                "bot": bot,
                                "bot_id": bot_id,
                                "connected_at": crate::durable::now_secs()
                            });
                            let _ = crate::config::atomic_json(&state_path, &state);
                        }
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
                        // Receipt log: every message the gateway sees, by ID
                        // and surface. Never the content — this is the only
                        // way to tell "never DM'd" from "event never arrived".
                        eprintln!("[discord] message from {} (dm: {is_dm})", m.author.id.get());
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
                            // A question card is waiting in this channel:
                            // the message is its answer, not a new turn.
                            if crate::ask::answer_typed(&store, &rest, &channel_id, &prompt).await {
                                continue;
                            }
                            let capacity = config
                                .get("queue_capacity")
                                .and_then(Value::as_u64)
                                .unwrap_or(1000);
                            let msg_id = m.id.to_string();
                            let conv = crate::session::conversation_key(
                                &channel_id,
                                &author_id,
                                is_dm,
                            );
                            // Burst guard: an over-eager user is told to slow
                            // down instead of forking gray per message.
                            let allowed_now = limiter
                                .lock()
                                .map(|mut l| l.allow(&author_id))
                                .unwrap_or(true);
                            if !allowed_now {
                                let wait = limiter
                                    .lock()
                                    .map(|mut l| l.retry_after_secs(&author_id))
                                    .unwrap_or(1);
                                if let Ok(ch) = channel_id.parse::<u64>() {
                                    let _ = rest
                                        .send_text_v2(
                                            ch,
                                            &format!("Too fast — try again in {wait}s."),
                                            None,
                                        )
                                        .await;
                                }
                                continue;
                            }
                            // Attachments the user sent: saved, then named in
                            // the prompt so gray's own tools can read them.
                            let mut prompt = prompt;
                            if !m.attachments.is_empty() {
                                let max_bytes = config
                                    .get("max_attachment_bytes")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(8 * 1024 * 1024);
                                let http = reqwest::Client::new();
                                let mut saved: Vec<std::path::PathBuf> = Vec::new();
                                for a in &m.attachments {
                                    let path = crate::attachments::save(
                                        &http,
                                        &token,
                                        &crate::attachments::AttachmentRef {
                                            url: &a.url,
                                            filename: &a.filename,
                                            size: a.size,
                                        },
                                        &msg_id,
                                        std::path::Path::new(&workdir),
                                        max_bytes,
                                    )
                                    .await;
                                    if let Some(p) = path {
                                        saved.push(p);
                                    }
                                }
                                if !saved.is_empty() {
                                    prompt = format!(
                                        "{}\n\n{}",
                                        prompt,
                                        crate::attachments::prompt_lines(&saved)
                                    );
                                }
                            }
                            if store
                                .enqueue(&msg_id, &channel_id, &prompt, Some(&conv), capacity)
                                .is_err()
                            {
                                if let Ok(ch) = channel_id.parse::<u64>() {
                                    let _ = rest
                                        .send_text_v2(
                                            ch,
                                            "Queue full or message invalid; this message was not accepted.",
                                            None,
                                        )
                                        .await;
                                }
                            }
                        } else if let Some(paired) =
                            crate::pairing::reply_for(&store, &author_id, is_dm, m.author.bot)
                        {
                            // Unknown human DMing the bot: tell them their own
                            // ID and mint a code — the owner approves it with
                            // `gray discord pairing approve discord <code>`.
                            if let Ok(ch) = channel_id.parse::<u64>() {
                                let embed =
                                    crate::pairing::unconfigured_embed(&author_id, &paired.code);
                                match crate::render::from_embed(&embed, &[]) {
                                    Ok(components) => match rest.send_v2(ch, &components, None).await {
                                        Ok(_) => eprintln!("[discord] pairing reply sent"),
                                        Err(e) => eprintln!("[discord] pairing reply failed: {e}"),
                                    },
                                    Err(e) => eprintln!("[discord] pairing render failed: {e}"),
                                }
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

                        let int_id = interaction.id.to_string();
                        // One context object for every handler; twilight
                        // never travels past this line.
                        let effective_app = app_id
                            .clone()
                            .unwrap_or_else(|| interaction.application_id.to_string());
                        let ctx = crate::command_dispatch::Ctx {
                            user_id: &user_id,
                            channel_id: &channel_id,
                            is_dm: interaction.guild_id.is_none(),
                            int_id: &int_id,
                            int_token: &interaction.token,
                            app_id: &effective_app,
                            rest: &rest,
                            store: &store,
                            config: &config,
                            config_path,
                        };
                        let is_component = matches!(
                            &interaction.data,
                            Some(
                                twilight_model::application::interaction::InteractionData::MessageComponent(_)
                                    | twilight_model::application::interaction::InteractionData::ModalSubmit(_)
                            )
                        );
                        let is_autocomplete = interaction.kind == InteractionType::ApplicationCommandAutocomplete;
                        // The turn card's Stop / Retry / New chat buttons are
                        // plugin-owned, not typed agent components: handle
                        // them before that router.
                        if let Some(twilight_model::application::interaction::InteractionData::MessageComponent(component)) = &interaction.data {
                            let id = component.custom_id.as_str();
                            if id.starts_with("turn:stop:") {
                                let pressed = interaction
                                    .message
                                    .as_ref()
                                    .and_then(|message| serde_json::to_value(&message.components).ok());
                                crate::command_dispatch::stop_button(&ctx, id, pressed.as_ref()).await;
                                continue;
                            }
                            if id.starts_with("turn:retry:") {
                                crate::command_dispatch::retry_button(&ctx, id).await;
                                continue;
                            }
                            if id.starts_with("turn:new:") {
                                crate::command_dispatch::new_chat_button(&ctx, id).await;
                                continue;
                            }
                            if id.starts_with("ask:") {
                                crate::command_dispatch::ask_press(&ctx, id, &component.values, None).await;
                                continue;
                            }
                        }
                        if let Some(twilight_model::application::interaction::InteractionData::ModalSubmit(modal)) = &interaction.data {
                            if modal.custom_id.starts_with("ask:") {
                                let raw = serde_json::to_value(&modal.components).unwrap_or(Value::Null);
                                let typed = find_value(&raw, "note");
                                crate::command_dispatch::ask_press(&ctx, &modal.custom_id, &[], typed.as_deref()).await;
                                continue;
                            }
                        }
                        if is_component || is_autocomplete {
                            let raw = serde_json::to_value(interaction)
                                .map_err(|_| "interaction could not be normalized".to_string())?;
                            let capacity = config
                                .get("queue_capacity")
                                .and_then(Value::as_u64)
                                .unwrap_or(1000);
                            let result = route_normalized_interaction(NormalizedRoute {
                                raw: &raw,
                                interaction_token: &interaction.token,
                                app_id: &effective_app,
                                rest: &rest,
                                store: &store,
                                file_store: file_store.as_ref(),
                                capacity,
                                is_dm: interaction.guild_id.is_none(),
                            })
                            .await;
                            if let Err(error) = result {
                                let legacy_id = raw
                                    .pointer("/data/custom_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                let legacy_handled = if legacy_id.starts_with("cron:remove:") {
                                    match &interaction.data {
                                        Some(twilight_model::application::interaction::InteractionData::MessageComponent(_component)) => {
                                            crate::command_dispatch::button(&ctx, legacy_id).await;
                                            true
                                        }
                                        Some(twilight_model::application::interaction::InteractionData::ModalSubmit(_component)) => {
                                            crate::command_dispatch::modal(&ctx, legacy_id).await;
                                            true
                                        }
                                        _ => false,
                                    }
                                } else {
                                    false
                                };
                                if !legacy_handled {
                                    if is_autocomplete {
                                        let _ = rest
                                            .autocomplete_response(&int_id, &interaction.token, &[])
                                            .await;
                                    } else {
                                        let error_components = crate::render::error_card(
                                            "This component is no longer available.",
                                        )
                                        .unwrap_or_else(|_| {
                                            vec![serde_json::json!({
                                                "type": 10,
                                                "content": "This component is no longer available."
                                            })]
                                        });
                                        let _ = rest
                                            .interaction_v2(
                                                &int_id,
                                                &interaction.token,
                                                &error_components,
                                                false,
                                            )
                                            .await;
                                    }
                                    eprintln!("[discord] component interaction rejected: {error}");
                                }
                            }
                            continue;
                        }

                        if let Some(twilight_model::application::interaction::InteractionData::ApplicationCommand(ref cmd)) = interaction.data {
                        // Flatten what Discord nests: a sub-command arrives as
                        // one option that carries its own options. Handlers
                        // read flat name/value pairs, so the parsing of
                        // twilight's shape lives here and nowhere else.
                        let (sub, args): (Option<String>, Vec<(&str, &str)>) = cmd
                            .options
                            .first()
                            .map(|opt| match &opt.value {
                                twilight_model::application::interaction::application_command::CommandOptionValue::SubCommand(inner) => (
                                    Some(opt.name.clone()),
                                    inner
                                        .iter()
                                        .filter_map(|o| match &o.value {
                                            twilight_model::application::interaction::application_command::CommandOptionValue::String(v) => Some((o.name.as_str(), v.as_str())),
                                            _ => None,
                                        })
                                        .collect(),
                                ),
                                _ => (None, Vec::new()),
                            })
                            .unwrap_or((None, Vec::new()));
                        // Top-level string options (the plain `/ask`).
                        let mut args = args;
                        for o in &cmd.options {
                            if let twilight_model::application::interaction::application_command::CommandOptionValue::String(v) = &o.value {
                                args.push((o.name.as_str(), v.as_str()));
                            }
                        }
                        match crate::commands::parse(cmd.name.as_str(), sub.as_deref(), &args) {
                            Some(request) => crate::command_dispatch::handle(&ctx, request).await,
                            None => {
                                // An interaction we did not register (or a
                                // sub-command that vanished under us).
                                eprintln!(
                                    "[discord] unparseable slash command: {} {sub:?}",
                                    cmd.name.as_str()
                                );
                                crate::command_dispatch::unknown(&ctx).await;
                            }
                        }
                        }
                        // Message-component and modal events are handled above
                        // through the typed state/event path. The old raw
                        // custom-id handlers remain available for legacy rows
                        // but are never selected for new interactions.
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

#[cfg(test)]
mod component_tests {
    use super::autocomplete_choices;
    use serde_json::json;

    #[test]
    fn autocomplete_choices_are_filtered_and_bounded() {
        let state = json!({"options": [
            {"label": "Gray Cloud", "value": "gray"},
            {"label": "Local", "value": "local"},
            {"label": "Other", "value": "other"}
        ]});
        let choices = autocomplete_choices(&json!({"query": "gr"}), &state);
        assert_eq!(
            choices,
            vec![("Gray Cloud".to_string(), "gray".to_string())]
        );
    }

    #[test]
    fn a_label_wrapped_text_box_survives_twilight() {
        // Discord's modal submit for the question card's text box.
        let submit = serde_json::json!({
            "custom_id": "ask:abc123:0:note",
            "components": [{
                "type": 18, "id": 1,
                "component": {"type": 4, "id": 2, "custom_id": "note", "value": "the hotfix branch"}
            }]
        });
        let data: twilight_model::application::interaction::modal::ModalInteractionData =
            serde_json::from_value(submit).unwrap();
        let raw = serde_json::to_value(&data.components).unwrap();
        assert_eq!(
            super::find_value(&raw, "note").as_deref(),
            Some("the hotfix branch")
        );
    }
}
