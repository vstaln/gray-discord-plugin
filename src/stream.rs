//! A turn as a stream of Discord messages, Hermes-style.
//!
//! With `GRAY_STREAM_TEXT=1`, gray's `--json` wire interleaves `text` rows
//! (the assistant's prose, numbered by segment, one segment per run of prose
//! between tool calls) with the tool progress rows. This module lays a turn
//! out the way Hermes' gateway does: every run of prose is its own message,
//! streamed in place, and every group of tool calls is its own progress
//! bubble below it. The next prose after a tool call starts a fresh message,
//! so the channel grows as the agent works instead of one message changing
//! out of sight:
//!
//! ```text
//! Let me check what's running on the box.            ← message 1 (prose)
//! -# Ran `gray ps` (0.3s)                             ← message 2 (tools)
//! -# Running `cargo test`
//! **Done / idle:** … ▉                               ← message 3 (prose)
//! ┃ -# Working · started 20 seconds ago · ran 2 commands [Stop]
//! ```
//!
//! The newest message carries the turn's status chip: a small Container
//! whose accent tracks the turn (blurple while working, green when done, red
//! on failure, grey when stopped) with the Stop button while it runs and
//! Retry / New chat once it has settled. Its clock is a Discord timestamp
//! (`<t:…:R>`) that every client keeps current on its own, so a quiet turn
//! costs no edits. When a newer message appears the chip moves down to it.
//!
//! The finished answer is the last message; the files it named (`MEDIA:`)
//! sit inside it. A tool group of more than a handful of lines folds to a
//! count plus its latest lines, with the full list in a spoiler below it.
//! A message too long for Discord continues in the next one.
//!
//! [`Timeline`] is the pure model. The gateway feeds it rows, asks it which
//! sends, edits and deletes are due ([`Timeline::plan`]), and reports back
//! what landed or failed. Clock and transport stay outside so the layout is
//! testable.

use serde_json::{json, Value};

/// Hermes' streaming cursor (`DEFAULT_STREAMING_CURSOR`).
pub const CURSOR: &str = " ▉";
/// Body text per message, in UTF-16 units. Discord allows 4000 across a V2
/// message; the rest is the status chip's and the tool log's.
pub const CARD_TEXT: usize = 3600;
/// Hermes' `MAX_SPLIT_MESSAGES`: one runaway run of prose never floods the
/// channel.
pub const MAX_SPLIT: usize = 8;
/// Messages one turn may open. Past it, later steps share the last one.
pub const MAX_MESSAGES: usize = 30;
/// A tool group longer than this folds into its tool log.
const FOLD_LINES: usize = 6;
/// Lines a folded group still shows, so the live action stays visible.
const KEEP_LIVE: usize = 2;
/// The tool log's share of a message's text.
const LOG_CHARS: usize = 900;
/// Discord's attachment cap per message.
pub const MAX_MEDIA: usize = 10;
/// Refused requests (HTTP 400) tolerated before a message is left as it is.
const STRIKES: u32 = 3;
/// Times a deleted newest message is posted again.
const REPOSTS: u32 = 2;

const WORKING: u32 = 0x5865F2;
const DONE: u32 = 0x57F287;
const FAILED: u32 = 0xED4245;
const STOPPED: u32 = 0x80848E;

/// Live replies on/off. Default on; `"stream_replies": false` keeps the
/// answer in one durable post at the end of the turn.
pub fn enabled(config: &Value) -> bool {
    config
        .get("stream_replies")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// Where the turn ended up; drives the accent and the status line.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Working,
    Done,
    /// A short reason: "failed", "timed out", "hit the budget".
    Failed(String),
    Stopped,
}

/// Why a send or edit did not land, as far as the turn cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Failure {
    /// Rate limited, a 5xx, a timeout or a dropped connection: try again
    /// after this many seconds.
    Busy(f64),
    /// Discord refused this payload (HTTP 400). The next frame differs, so
    /// it is retried with backoff, a few times.
    Refused,
    /// The message is gone (deleted by someone, HTTP 404).
    Gone,
    /// The bot may not post or edit here (HTTP 401/403).
    Forbidden,
}

#[derive(Debug, Clone)]
enum Block {
    Text {
        /// Append-only lines from `delta`s.
        stable: String,
        /// The provisional unfinished line from the latest row.
        tail: String,
        segment: u64,
        done: bool,
        /// The authoritative answer once the turn has finished. Already
        /// stripped of delivered `MEDIA:` tags.
        answer: Option<String>,
    },
    Tools {
        rows: Vec<Value>,
    },
}

/// One Discord message the timeline owns.
#[derive(Debug, Clone, Default)]
struct Posted {
    id: String,
    shown: String,
    at: f64,
    /// Refused edits in a row.
    strikes: u32,
    /// No edit before this time (backoff after a failure).
    retry_at: f64,
    /// Left as it is for the rest of the turn.
    frozen: bool,
}

/// One message as it should look now.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub components: Vec<Value>,
    /// The message's body as plain markdown (no status chip): what the
    /// durable outbox records for it, and what the activity hook reports.
    pub text: String,
    /// Uploads this message references as `attachment://<name>`; a send or
    /// edit must carry them.
    pub files: Vec<String>,
    /// The last block of the turn this message shows.
    pub block: usize,
}

/// A file the finished answer named, shown inside its message.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub name: String,
    /// Images and videos go to the gallery; anything else is a File card.
    pub visual: bool,
}

impl Card {
    fn key(&self) -> String {
        serde_json::to_string(&self.components).unwrap_or_default()
    }
}

/// One request the gateway should make, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Post { card: usize, body: Card },
    Edit { card: usize, id: String, body: Card },
    Delete { card: usize, id: String },
}

/// The finished answer as it landed: each of its messages' body and id.
#[derive(Debug, Clone, PartialEq)]
pub struct Landed {
    pub parts: Vec<(String, String)>,
}

/// One block's share of the layout before it is cut into messages.
struct Unit {
    text: String,
    log: Option<(Value, usize)>,
    block: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Timeline {
    blocks: Vec<Block>,
    cards: Vec<Posted>,
    /// Messages cannot be posted here at all (forbidden, or refused again
    /// and again). The turn is left alone from then on.
    lost: bool,
    /// Refused posts in a row, and no post before `post_retry`.
    post_strikes: u32,
    post_retry: f64,
    reposts: u32,
    /// Stream prose for this turn. Off for slash-command turns (their
    /// answer belongs to the interaction) and when `stream_replies` is off;
    /// `text` rows are then ignored and only tool bubbles show.
    text: bool,
    started: f64,
    /// How the turn ended, and when: the status line's time stops there.
    status: Option<(Status, f64)>,
    /// `custom_id` of the status chip's Stop button while the turn runs.
    stop: Option<String>,
    /// `custom_id`s of the settled chip's Retry and New chat buttons.
    retry: Option<String>,
    new_chat: Option<String>,
    media: Vec<Media>,
}

impl Timeline {
    pub fn new(text: bool, started: f64) -> Self {
        Self {
            text,
            started,
            ..Self::default()
        }
    }

    /// Offer a Stop button in the status chip while the turn runs.
    pub fn with_stop(mut self, custom_id: String) -> Self {
        self.stop = Some(custom_id);
        self
    }

    /// Offer Retry and New chat buttons once the turn has settled.
    pub fn with_actions(mut self, retry: Option<String>, new_chat: Option<String>) -> Self {
        self.retry = retry;
        self.new_chat = new_chat;
        self
    }

    /// Show the answer's files inside its message (at most [`MAX_MEDIA`]).
    pub fn attach(&mut self, media: Vec<Media>) {
        self.media = media.into_iter().take(MAX_MEDIA).collect();
    }

    /// Fold one progress row into the layout.
    pub fn absorb(&mut self, row: &Value) {
        if row.get("phase").and_then(Value::as_str) == Some("text") {
            if self.text {
                self.absorb_text(row);
            }
            return;
        }
        if let Some(Block::Tools { rows }) = self.blocks.last_mut() {
            rows.push(row.clone());
            return;
        }
        if !crate::activity::narrates(row) {
            return;
        }
        // A tool after prose closes it, even if gray's closing row was lost.
        self.close_text();
        self.blocks.push(Block::Tools {
            rows: vec![row.clone()],
        });
    }

    fn absorb_text(&mut self, row: &Value) {
        let segment = row.get("segment").and_then(Value::as_u64).unwrap_or(0);
        let delta = row.get("delta").and_then(Value::as_str).unwrap_or("");
        let new_tail = row.get("tail").and_then(Value::as_str).unwrap_or("");
        let closes = row.get("done").and_then(Value::as_bool).unwrap_or(false);
        let index = self
            .blocks
            .iter()
            .rposition(|block| matches!(block, Block::Text { segment: s, .. } if *s == segment));
        let index = match index {
            Some(index) => index,
            None => {
                self.close_text();
                self.blocks.push(Block::Text {
                    segment,
                    stable: String::new(),
                    tail: String::new(),
                    done: false,
                    answer: None,
                });
                self.blocks.len() - 1
            }
        };
        if let Block::Text {
            stable, tail, done, ..
        } = &mut self.blocks[index]
        {
            stable.push_str(delta);
            *tail = new_tail.to_string();
            *done |= closes;
        }
    }

    fn close_text(&mut self) {
        for block in &mut self.blocks {
            if let Block::Text { done, .. } = block {
                *done = true;
            }
        }
    }

    /// Whether any prose has been streamed this turn.
    pub fn has_text(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, Block::Text { .. }))
    }

    /// Hand the finished turn's authoritative answer (prose only, `MEDIA:`
    /// tags already taken out) to the turn. When the turn's last message is
    /// the prose it streamed as, that message becomes the answer in place;
    /// otherwise (the agent's last act was a tool call, or this gray streams
    /// no prose) the answer is the next new message below the tool lines.
    /// `false` when the caller must deliver the answer itself: prose
    /// streaming is off for this turn, the turn cannot post here, or nothing
    /// was shown before the answer (it is then one plain durable post).
    pub fn adopt(&mut self, prose: &str) -> bool {
        if self.lost || !self.text || prose.trim().is_empty() {
            return false;
        }
        match self.blocks.last_mut() {
            None => return false,
            Some(Block::Text { done, answer, .. }) => {
                *done = true;
                *answer = Some(prose.to_string());
            }
            Some(Block::Tools { .. }) => {
                self.close_text();
                self.blocks.push(Block::Text {
                    segment: u64::MAX,
                    stable: String::new(),
                    tail: String::new(),
                    done: true,
                    answer: Some(prose.to_string()),
                });
            }
        }
        true
    }

    /// Record how the turn ended at `now`. The next plan settles the turn.
    pub fn settle(&mut self, status: Status, now: f64) {
        self.status = Some((status, now));
    }

    /// The messages this turn should show at `now`.
    pub fn render(&self, now: f64) -> Vec<Card> {
        self.render_with(now, true)
    }

    fn render_with(&self, now: f64, buttons: bool) -> Vec<Card> {
        let (status, now) = self.status.clone().unwrap_or((Status::Working, now));
        let live = status == Status::Working;
        let mut units: Vec<Unit> = Vec::new();
        let mut tool_rows: Vec<Value> = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            let unit = match block {
                Block::Tools { rows } => {
                    tool_rows.extend(rows.iter().cloned());
                    tool_unit(rows)
                }
                Block::Text {
                    stable,
                    tail,
                    done,
                    answer,
                    ..
                } => {
                    let prose = match answer {
                        Some(answer) => answer.clone(),
                        None => crate::media_tags::strip_for_display(&format!("{stable}{tail}")),
                    };
                    let prose = crate::text::sanitize(&prose);
                    let prose = prose.trim_end();
                    if prose.trim().is_empty() {
                        None
                    } else if live && !done {
                        Some((format!("{prose}{CURSOR}"), None))
                    } else {
                        Some((prose.to_string(), None))
                    }
                }
            };
            if let Some((text, log)) = unit {
                units.push(Unit {
                    text,
                    log,
                    block: index,
                });
            }
        }
        if units.is_empty() {
            return Vec::new();
        }
        // A turn of dozens of steps shares one last message rather than
        // flooding the channel.
        if units.len() > MAX_MESSAGES {
            let tail = units.split_off(MAX_MESSAGES - 1);
            let block = tail.last().map_or(0, |unit| unit.block);
            let text = tail
                .iter()
                .map(|unit| unit.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            units.push(Unit {
                text,
                log: None,
                block,
            });
        }
        let settled = status != Status::Working;
        let chip = self.chip(
            &status,
            &crate::activity::summary(&tool_rows),
            now,
            buttons && settled,
        );
        let mut cards: Vec<Card> = Vec::new();
        for unit in units {
            let log_size = unit.log.as_ref().map_or(0, |(_, size)| *size);
            let chunks = split_block(&unit.text, CARD_TEXT.saturating_sub(log_size));
            let last = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.into_iter().enumerate() {
                let mut components = vec![json!({"type": 10, "content": chunk})];
                if index == last {
                    components.extend(unit.log.clone().map(|(container, _)| container));
                }
                cards.push(Card {
                    components,
                    text: chunk,
                    files: Vec::new(),
                    block: unit.block,
                });
            }
        }
        if let Some(last) = cards.last_mut() {
            last.components.extend(self.media_components());
            last.files = self.media.iter().map(|media| media.name.clone()).collect();
            last.components.push(chip);
        }
        cards
    }

    /// The answer's files: images and videos in one gallery, the rest as
    /// File cards, all referencing uploads that travel with the request.
    fn media_components(&self) -> Vec<Value> {
        let mut out = Vec::new();
        let visuals: Vec<Value> = self
            .media
            .iter()
            .filter(|media| media.visual)
            .map(|media| json!({"media": {"url": format!("attachment://{}", media.name)}}))
            .collect();
        if !visuals.is_empty() {
            out.push(json!({"type": 12, "items": visuals}));
        }
        for media in self.media.iter().filter(|media| !media.visual) {
            out.push(json!({"type": 13, "file": {"url": format!("attachment://{}", media.name)}}));
        }
        out
    }

    fn action_row(&self) -> Option<Value> {
        let mut buttons = Vec::new();
        if let Some(custom_id) = &self.retry {
            buttons.push(json!({
                "type": 2, "style": 2, "label": "Retry", "custom_id": custom_id,
            }));
        }
        if let Some(custom_id) = &self.new_chat {
            buttons.push(json!({
                "type": 2, "style": 2, "label": "New chat", "custom_id": custom_id,
            }));
        }
        (!buttons.is_empty()).then(|| json!({"type": 1, "components": buttons}))
    }

    /// The status chip: an accented Container with the status line, Stop
    /// while the turn runs, and Retry / New chat once it has settled.
    fn chip(&self, status: &Status, summary: &str, now: f64, actions: bool) -> Value {
        let elapsed = (now - self.started).max(0.0);
        let mut line = match status {
            // A Discord timestamp: "started 20 seconds ago", kept current by
            // every client with no edits from us.
            Status::Working => {
                format!("Working · started <t:{}:R>", self.started.max(0.0) as i64)
            }
            Status::Done => format!("Done in {}", duration(elapsed)),
            Status::Failed(reason) => {
                format!("{} after {}", capitalized(reason), duration(elapsed))
            }
            Status::Stopped => format!("Stopped after {}", duration(elapsed)),
        };
        if !summary.is_empty() {
            line.push_str(" · ");
            line.push_str(summary);
        }
        if matches!(status, Status::Failed(_) | Status::Stopped) {
            line.push_str(" · actions may already have happened");
        }
        let text = json!({"type": 10, "content": format!("-# {line}")});
        let first = match (&self.stop, status) {
            (Some(custom_id), Status::Working) => json!({
                "type": 9,
                "components": [text],
                "accessory": {
                    "type": 2,
                    "style": 4,
                    "label": "Stop",
                    "custom_id": custom_id,
                },
            }),
            _ => text,
        };
        let mut children = vec![first];
        if actions {
            children.extend(self.action_row());
        }
        let accent = match status {
            Status::Working => WORKING,
            Status::Done => DONE,
            Status::Failed(_) => FAILED,
            Status::Stopped => STOPPED,
        };
        json!({"type": 17, "accent_color": accent, "components": children})
    }

    /// The requests due now. Posts go strictly in message order; an edit
    /// waits `gap` seconds after the message's last change (and out any
    /// backoff) unless the turn is `finishing`, when everything settles at
    /// once.
    pub fn plan(&self, now: f64, gap: f64, finishing: bool) -> Vec<Op> {
        if self.lost {
            return Vec::new();
        }
        let cards = self.render(now);
        let mut ops = Vec::new();
        for (index, body) in cards.iter().enumerate() {
            match self.cards.get(index) {
                Some(posted) if posted.frozen => {}
                Some(posted) => {
                    let due = finishing || (now - posted.at >= gap && now >= posted.retry_at);
                    if posted.shown != body.key() && due {
                        ops.push(Op::Edit {
                            card: index,
                            id: posted.id.clone(),
                            body: body.clone(),
                        });
                    }
                }
                None => {
                    if finishing || now >= self.post_retry {
                        ops.push(Op::Post {
                            card: index,
                            body: body.clone(),
                        });
                    }
                }
            }
        }
        // The turn shrank (the final answer is shorter than its preview):
        // retire the messages it no longer fills.
        if finishing {
            for (index, posted) in self.cards.iter().enumerate().skip(cards.len()) {
                ops.push(Op::Delete {
                    card: index,
                    id: posted.id.clone(),
                });
            }
        }
        ops
    }

    /// A post landed as message `id`.
    pub fn posted(&mut self, card: usize, id: &str, body: &Card, now: f64) {
        self.post_strikes = 0;
        if card == self.cards.len() {
            self.cards.push(Posted {
                id: id.to_string(),
                shown: body.key(),
                at: now,
                ..Posted::default()
            });
        }
    }

    /// An edit landed.
    pub fn edited(&mut self, card: usize, body: &Card, now: f64) {
        if let Some(posted) = self.cards.get_mut(card) {
            posted.shown = body.key();
            posted.at = now;
            posted.strikes = 0;
        }
    }

    /// A post did not land. A busy Discord is waited out; a refused post is
    /// retried a few times (the next frame differs); a forbidden channel
    /// ends the turn's messages.
    pub fn post_failed(&mut self, failure: Failure, now: f64) {
        match failure {
            Failure::Busy(secs) => self.post_retry = now + secs.clamp(0.5, 30.0),
            Failure::Refused => {
                self.post_strikes += 1;
                if self.post_strikes >= STRIKES {
                    self.lost = true;
                } else {
                    self.post_retry = now + backoff(self.post_strikes);
                }
            }
            Failure::Gone | Failure::Forbidden => self.lost = true,
        }
    }

    /// An edit did not land. Only that message is affected: a busy Discord
    /// is waited out, a refused edit is retried with backoff and then left
    /// as it is, and a deleted newest message is posted again.
    pub fn edit_failed(&mut self, card: usize, failure: Failure, now: f64) {
        let newest = card + 1 == self.cards.len();
        let reposts = self.reposts;
        let Some(posted) = self.cards.get_mut(card) else {
            return;
        };
        match failure {
            Failure::Busy(secs) => posted.retry_at = now + secs.clamp(0.5, 30.0),
            Failure::Refused => {
                posted.strikes += 1;
                if posted.strikes >= STRIKES {
                    posted.frozen = true;
                } else {
                    posted.retry_at = now + backoff(posted.strikes);
                }
            }
            Failure::Gone if newest && reposts < REPOSTS => {
                self.reposts += 1;
                self.cards.truncate(card);
            }
            Failure::Gone | Failure::Forbidden => posted.frozen = true,
        }
    }

    /// A message was deleted (by us, at the end of the turn).
    pub fn deleted(&mut self, card: usize) {
        self.cards.truncate(card);
    }

    /// Give the turn up: nothing more is sent or edited.
    pub fn lose(&mut self) {
        self.lost = true;
    }

    /// Whether the newest message is on screen exactly as rendered at `now`.
    fn newest_shown(&self, now: f64) -> Option<(Card, String)> {
        if self.lost {
            return None;
        }
        let cards = self.render(now);
        let index = cards.len().checked_sub(1)?;
        let posted = self.cards.get(index)?;
        let card = cards.into_iter().nth(index)?;
        (posted.shown == card.key()).then(|| (card, posted.id.clone()))
    }

    /// The index of the adopted answer's block, if there is one.
    fn answer_block(&self) -> Option<usize> {
        match self.blocks.last() {
            Some(Block::Text {
                answer: Some(_), ..
            }) => Some(self.blocks.len() - 1),
            _ => None,
        }
    }

    /// The adopted answer, if every message of it is on screen exactly as
    /// rendered. `None` means the caller must deliver the answer itself.
    pub fn landed(&self, now: f64) -> Option<Landed> {
        let answer = self.answer_block()?;
        if self.lost {
            return None;
        }
        let cards = self.render(now);
        if self.cards.len() > cards.len() {
            return None;
        }
        let mut parts = Vec::new();
        for (index, card) in cards.iter().enumerate() {
            if card.block != answer {
                continue;
            }
            let posted = self.cards.get(index)?;
            if posted.shown != card.key() {
                return None;
            }
            parts.push((card.text.clone(), posted.id.clone()));
        }
        (!parts.is_empty()).then_some(Landed { parts })
    }

    /// The message whose status chip shows how the turn ended, if it is on
    /// screen: a failed or stopped turn needs no separate notice when its
    /// chip says so.
    pub fn status_card(&self, now: f64) -> Option<String> {
        self.status.as_ref()?;
        self.newest_shown(now).map(|(_, id)| id)
    }

    /// The settled newest message without its Retry / New chat buttons, for
    /// when the next turn starts: only the latest turn offers them. `None`
    /// when there is nothing to retire, or the message carries uploads (an
    /// edit would have to resend them).
    pub fn retired(&self) -> Option<(String, Vec<Value>)> {
        self.status.as_ref()?;
        if (self.retry.is_none() && self.new_chat.is_none()) || !self.media.is_empty() {
            return None;
        }
        let (_, id) = self.newest_shown(0.0)?;
        let last = self.render_with(0.0, false).pop()?;
        Some((id, last.components))
    }

    /// The answer's messages on screen, for retracting a preview whose
    /// answer did not land before the durable resend.
    pub fn answer_ids(&self) -> Vec<String> {
        let Some(answer) = self.answer_block() else {
            return Vec::new();
        };
        let cards = self.render(0.0);
        self.cards
            .iter()
            .enumerate()
            .filter(|(index, _)| cards.get(*index).is_none_or(|card| card.block == answer))
            .map(|(_, posted)| posted.id.clone())
            .collect()
    }
}

/// One tool group's lines, folded when long, and its tool log.
fn tool_unit(rows: &[Value]) -> Option<(String, Option<(Value, usize)>)> {
    let feed = crate::activity::render(rows)?;
    let feed = crate::text::sanitize(&feed);
    let lines: Vec<&str> = feed.lines().collect();
    let (shown, log) = if lines.len() > FOLD_LINES {
        let mut shown = vec![format!(
            "{} tool calls · full list in the tool log below",
            lines.len()
        )];
        shown.extend(
            lines[lines.len() - KEEP_LIVE..]
                .iter()
                .map(|line| line.to_string()),
        );
        let all: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        (shown, tool_log(&all))
    } else {
        (lines.iter().map(|line| line.to_string()).collect(), None)
    };
    let text = shown
        .iter()
        .map(|line| format!("-# {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some((text, log))
}

/// Seconds to wait after the `strikes`-th refusal: 2, 4, 8 … capped at 30.
fn backoff(strikes: u32) -> f64 {
    2f64.powi(strikes.min(5) as i32).min(30.0)
}

/// "timed out" → "Timed out".
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `4.1s`, `1m 12s`.
fn duration(secs: f64) -> String {
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    let whole = secs as u64;
    format!("{}m {:02}s", whole / 60, whole % 60)
}

/// The spoiler container holding every line of the folded tool groups, and
/// its text size. Bounded: the newest lines win, older ones become a count.
fn tool_log(lines: &[String]) -> Option<(Value, usize)> {
    if lines.is_empty() {
        return None;
    }
    let mut kept: Vec<String> = Vec::new();
    let mut size = 0usize;
    for line in lines.iter().rev() {
        let entry = format!("-# {line}");
        let cost = crate::text::utf16_len(&entry) + 1;
        if size + cost > LOG_CHARS - 40 {
            break;
        }
        size += cost;
        kept.push(entry);
    }
    kept.reverse();
    let dropped = lines.len() - kept.len();
    if dropped > 0 {
        kept.insert(0, format!("-# … +{dropped} earlier"));
    }
    let head = format!("-# Tool log · {} calls", lines.len());
    let body = kept.join("\n");
    let size = crate::text::utf16_len(&head) + crate::text::utf16_len(&body);
    Some((
        json!({
            "type": 17,
            "spoiler": true,
            "components": [
                {"type": 10, "content": head},
                {"type": 10, "content": body},
            ],
        }),
        size,
    ))
}

/// The pressed message, flipped to "stopping…" for the interaction's own
/// UPDATE_MESSAGE response: grey accent, the Stop section replaced by a
/// line. The worker settles the turn properly on its next frame. `None`
/// when the button is not on this message.
pub fn stopping(components: &Value, pressed: &str) -> Option<Vec<Value>> {
    let mut out = components.as_array()?.clone();
    let mut found = false;
    for top in &mut out {
        strip_nulls(top);
        if top.get("type").and_then(Value::as_u64) != Some(17)
            || top.get("spoiler").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        top["accent_color"] = json!(STOPPED);
        let Some(children) = top.get_mut("components").and_then(Value::as_array_mut) else {
            continue;
        };
        for child in children.iter_mut() {
            let is_stop = child.get("type").and_then(Value::as_u64) == Some(9)
                && child
                    .pointer("/accessory/custom_id")
                    .and_then(Value::as_str)
                    == Some(pressed);
            if is_stop {
                *child = json!({"type": 10, "content": "-# Stopping…"});
                found = true;
            }
        }
    }
    found.then_some(out)
}

/// Discord (and twilight) echo optional fields as `null`; the validator
/// reads a present key as set, so drop them before sending a tree back.
fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, field| !field.is_null());
            for field in map.values_mut() {
                strip_nulls(field);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// Cut one block's text into messages of at most `budget` UTF-16 units
/// (Hermes seals an overflowing preview the same way), so text never moves
/// between messages as it grows: a cut lands on the last newline that keeps
/// at least half the room, and a code fence open at the cut is closed there
/// and reopened in the next message. At most [`MAX_SPLIT`] messages.
pub fn split_block(text: &str, budget: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut rest = text.to_string();
    while !rest.trim().is_empty() {
        if crate::text::utf16_len(&rest) <= budget {
            chunks.push(rest);
            break;
        }
        if chunks.len() + 1 == MAX_SPLIT {
            let (head, _) = split_at(&rest, budget.saturating_sub(24));
            chunks.push(format!("{head}\n-# … (truncated)"));
            break;
        }
        let (head, tail) = split_at(&rest, budget);
        chunks.push(head);
        rest = tail;
    }
    chunks.retain(|chunk| !chunk.trim().is_empty());
    chunks
}

/// Cut `text` to fit `room`, returning the head and what remains.
fn split_at(text: &str, room: usize) -> (String, String) {
    // Room for a closing fence below the cut.
    let budget = room.saturating_sub(4).max(2);
    let head = crate::text::prefix_within_limit(text, budget);
    let cut = match head.rfind('\n') {
        Some(at) if at > 0 && at >= head.len() / 2 => at,
        _ => head.len(),
    };
    let (closed, open) = close_fence(&text[..cut]);
    let rest = text[cut..].trim_start_matches('\n');
    let rest = match open {
        Some(fence) => format!("{fence}\n{rest}"),
        None => rest.to_string(),
    };
    (closed, rest)
}

/// Close a code fence left open at the end of `chunk`; returns the chunk
/// and the fence line to reopen, if any.
fn close_fence(chunk: &str) -> (String, Option<String>) {
    let mut open: Option<&str> = None;
    for line in chunk.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            open = match open {
                Some(_) => None,
                None => Some(trimmed),
            };
        }
    }
    match open {
        Some(fence) => (format!("{chunk}\n```"), Some(fence.to_string())),
        None => (chunk.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(segment: u64, delta: &str, tail: &str, done: bool) -> Value {
        json!({"phase": "text", "segment": segment, "delta": delta, "tail": tail, "done": done})
    }

    fn ran(id: &str, command: &str) -> Value {
        json!({"phase": "tool_ran", "call_id": id, "tool": "bash", "detail": command})
    }

    /// Apply every op as if Discord accepted it.
    fn land(timeline: &mut Timeline, now: f64, finishing: bool) -> Vec<Op> {
        let ops = timeline.plan(now, 1.0, finishing);
        for op in &ops {
            match op {
                Op::Post { card, body } => {
                    let id = format!("m{card}");
                    timeline.posted(*card, &id, body, now);
                }
                Op::Edit { card, body, .. } => timeline.edited(*card, body, now),
                Op::Delete { card, .. } => timeline.deleted(*card),
            }
        }
        ops
    }

    /// The status chip: the last component of the newest message.
    fn chip(card: &Card) -> &Value {
        card.components.last().unwrap()
    }

    fn status_line(card: &Card) -> String {
        let first = &chip(card)["components"][0];
        match first["type"].as_u64() {
            Some(9) => first["components"][0]["content"]
                .as_str()
                .unwrap()
                .to_string(),
            _ => first["content"].as_str().unwrap().to_string(),
        }
    }

    #[test]
    fn prose_streams_into_its_own_message_with_the_status_chip() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&text(0, "", "Hello", false));
        let ops = land(&mut t, 0.0, false);
        let [Op::Post { body, .. }] = &ops[..] else {
            panic!("{ops:?}")
        };
        crate::render::validate_components(&body.components).unwrap();
        assert_eq!(body.text, "Hello ▉");
        assert_eq!(body.components[0]["type"], 10, "prose is plain text");
        assert_eq!(chip(body)["type"], 17);
        assert_eq!(chip(body)["accent_color"], json!(WORKING));
        let stop = &chip(body)["components"][0];
        assert_eq!(stop["type"], 9, "the status line carries Stop");
        assert_eq!(stop["accessory"]["custom_id"], "turn:stop:t");

        t.absorb(&text(0, "Hello world\n", "and", false));
        assert!(t.plan(0.5, 1.0, false).is_empty(), "inside the edit gap");
        let ops = land(&mut t, 1.0, false);
        assert!(
            matches!(&ops[..], [Op::Edit { body, .. }] if body.text == "Hello world\nand ▉"),
            "{ops:?}"
        );
    }

    #[test]
    fn each_step_of_the_turn_is_a_new_message() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&text(0, "Let me check.", "", false));
        land(&mut t, 0.0, false);
        t.absorb(&ran("a", "cargo test"));
        let ops = land(&mut t, 2.0, false);
        let [Op::Edit {
            card: 0,
            body: prose,
            ..
        }, Op::Post {
            card: 1,
            body: tools,
        }] = &ops[..]
        else {
            panic!("{ops:?}")
        };
        assert_eq!(prose.text, "Let me check.", "closed: cursor gone");
        assert_eq!(prose.components.len(), 1, "the chip moved on");
        assert_eq!(tools.text, "-# Running `cargo test`");
        assert_eq!(
            status_line(tools),
            "-# Working · started <t:0:R> · ran 1 command"
        );

        t.absorb(&text(1, "", "All", false));
        let ops = land(&mut t, 4.0, false);
        assert!(
            matches!(&ops[..], [Op::Edit { card: 1, .. }, Op::Post { card: 2, body }] if body.text == "All ▉"),
            "the next prose opens a third message: {ops:?}"
        );
        assert_eq!(t.cards.len(), 3);
    }

    #[test]
    fn the_live_clock_never_costs_an_edit() {
        let mut t = Timeline::new(true, 1_700_000_000.0);
        t.absorb(&text(0, "", "Hi", false));
        land(&mut t, 1_700_000_000.0, false);
        assert_eq!(
            status_line(&t.render(1_700_000_000.0)[0]),
            "-# Working · started <t:1700000000:R>",
            "Discord keeps the relative time current itself"
        );
        assert!(
            t.plan(1_700_000_600.0, 1.0, false).is_empty(),
            "ten idle minutes, no edits"
        );
    }

    #[test]
    fn a_long_tool_group_folds_into_a_spoiler_log() {
        let mut t = Timeline::new(true, 0.0);
        for i in 0..9 {
            t.absorb(&ran(&format!("c{i}"), &format!("step {i}")));
        }
        let card = &t.render(0.0)[0];
        crate::render::validate_components(&card.components).unwrap();
        assert_eq!(
            card.text,
            "-# 9 tool calls · full list in the tool log below\n\
             -# Running `step 7`\n-# Running `step 8`"
        );
        let log = &card.components[1];
        assert_eq!(log["type"], 17);
        assert_eq!(log["spoiler"], true, "tap to reveal");
        let lines = log["components"][1]["content"].as_str().unwrap();
        assert!(lines.starts_with("-# Running `step 0`"), "{lines}");
        assert_eq!(lines.lines().count(), 9);
    }

    #[test]
    fn settling_recolors_the_chip_and_drops_the_stop_button() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&ran("a", "cargo test"));
        t.absorb(&text(0, "", "Draft", false));
        land(&mut t, 0.0, false);
        assert_eq!(t.cards.len(), 2);
        assert!(t.adopt("Final answer"));
        t.settle(Status::Done, 4.1);
        let ops = land(&mut t, 9.0, true);
        let [Op::Edit { card: 1, body, .. }] = &ops[..] else {
            panic!("{ops:?}")
        };
        assert_eq!(chip(body)["accent_color"], json!(DONE));
        assert_eq!(status_line(body), "-# Done in 4.1s · ran 1 command");
        assert!(!body.key().contains("turn:stop"));
        let landed = t.landed(30.0).unwrap();
        assert_eq!(
            landed.parts,
            vec![("Final answer".to_string(), "m1".to_string())],
            "only the answer's messages are the answer"
        );
    }

    #[test]
    fn an_answer_after_tool_lines_is_a_new_message_below_them() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "Checking", "", true));
        t.absorb(&ran("a", "ls -la"));
        land(&mut t, 0.0, false);
        assert!(t.adopt("Answer"));
        t.settle(Status::Done, 1.0);
        let ops = land(&mut t, 1.0, true);
        assert!(
            matches!(&ops[..], [Op::Edit { card: 1, .. }, Op::Post { card: 2, body }] if body.text == "Answer"),
            "{ops:?}"
        );
        assert_eq!(
            t.landed(1.0).unwrap().parts,
            vec![("Answer".to_string(), "m2".to_string())]
        );
    }

    #[test]
    fn a_settled_turn_offers_retry_and_new_chat_and_retires_them_later() {
        let mut t = Timeline::new(true, 0.0)
            .with_stop("turn:stop:s".into())
            .with_actions(Some("turn:retry:r".into()), Some("turn:new:n".into()));
        t.absorb(&text(0, "", "Hi", false));
        land(&mut t, 0.0, false);
        let live = t.render(0.0);
        assert!(!live[0].key().contains("turn:retry"));
        t.adopt("Hi there");
        t.settle(Status::Done, 2.0);
        land(&mut t, 2.0, true);
        let done = t.render(2.0);
        crate::render::validate_components(&done[0].components).unwrap();
        let row = chip(&done[0])["components"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(row["type"], 1, "an action row closes the settled chip");
        assert_eq!(row["components"][0]["custom_id"], "turn:retry:r");
        assert_eq!(row["components"][1]["custom_id"], "turn:new:n");
        assert_eq!(row["components"][1]["label"], "New chat");

        let (id, components) = t.retired().unwrap();
        assert_eq!(id, "m0");
        let flat = serde_json::to_string(&components).unwrap();
        assert!(!flat.contains("turn:retry") && !flat.contains("turn:new"));
        assert!(flat.contains("Done in 2.0s"), "only the buttons go");
    }

    #[test]
    fn the_answer_files_sit_inside_its_message() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "", "Here", false));
        land(&mut t, 0.0, false);
        t.adopt("Here it is:");
        t.attach(vec![
            Media {
                name: "chart.png".into(),
                visual: true,
            },
            Media {
                name: "report.pdf".into(),
                visual: false,
            },
        ]);
        t.settle(Status::Done, 1.0);
        let ops = land(&mut t, 1.0, true);
        let [Op::Edit { body, .. }] = &ops[..] else {
            panic!("{ops:?}")
        };
        crate::render::validate_components(&body.components).unwrap();
        assert_eq!(body.files, vec!["chart.png", "report.pdf"]);
        assert_eq!(body.components[1]["type"], 12, "gallery after the prose");
        assert_eq!(
            body.components[1]["items"][0]["media"]["url"],
            "attachment://chart.png"
        );
        assert_eq!(body.components[2]["type"], 13, "then the file card");
        assert!(t.landed(1.0).is_some());
        assert!(
            t.retired().is_none(),
            "a message with uploads is left as is"
        );
    }

    #[test]
    fn stop_flips_the_pressed_message_at_once() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:s".into());
        t.absorb(&text(0, "", "Working", false));
        let mut echoed = json!(t.render(0.0)[0].components);
        // Discord echoes optional fields back as null.
        echoed[0]["id"] = Value::Null;
        let flipped = stopping(&echoed, "turn:stop:s").unwrap();
        crate::render::validate_components(&flipped).unwrap();
        assert_eq!(flipped.last().unwrap()["accent_color"], json!(STOPPED));
        let text = serde_json::to_string(&flipped).unwrap();
        assert!(text.contains("Stopping…") && !text.contains("turn:stop:s"));
        assert!(stopping(&echoed, "turn:stop:other").is_none());
    }

    #[test]
    fn a_stopped_turn_says_so_in_its_chip() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&text(0, "", "Working on", false));
        land(&mut t, 0.0, false);
        t.settle(Status::Stopped, 12.0);
        land(&mut t, 12.5, true);
        let card = &t.render(99.0)[0];
        assert_eq!(chip(card)["accent_color"], json!(STOPPED));
        assert_eq!(
            status_line(card),
            "-# Stopped after 12.0s · actions may already have happened"
        );
        assert_eq!(card.text, "Working on", "the cursor is gone");
        assert_eq!(t.status_card(99.0).as_deref(), Some("m0"));
    }

    #[test]
    fn prose_rows_are_ignored_when_streaming_is_off() {
        let mut t = Timeline::new(false, 0.0);
        t.absorb(&text(0, "secret plan", "", true));
        assert!(!t.has_text());
        assert!(t.plan(0.0, 1.0, true).is_empty());
        assert!(!t.adopt("answer"), "the answer belongs to someone else");
    }

    #[test]
    fn a_lost_turn_is_left_alone_and_never_lands() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "", "Hi", false));
        t.lose();
        assert!(t.plan(5.0, 1.0, true).is_empty());
        assert!(!t.adopt("Hi there"));
        assert!(t.landed(5.0).is_none());
    }

    #[test]
    fn a_refused_edit_backs_off_then_leaves_only_that_message() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "", "One", false));
        land(&mut t, 0.0, false);
        t.absorb(&text(0, "One two\n", "", false));
        let [Op::Edit { card: 0, .. }] = &t.plan(1.0, 1.0, false)[..] else {
            panic!()
        };
        t.edit_failed(0, Failure::Refused, 1.0);
        assert!(t.plan(2.0, 1.0, false).is_empty(), "backing off");
        assert_eq!(t.plan(3.0, 1.0, false).len(), 1, "then tried again");
        t.edit_failed(0, Failure::Refused, 3.0);
        t.edit_failed(0, Failure::Refused, 30.0);
        assert!(t.plan(60.0, 1.0, false).is_empty(), "left as it is");

        // The turn goes on: the next step still lands as a new message.
        t.absorb(&ran("a", "cargo build"));
        let ops = land(&mut t, 61.0, false);
        assert!(matches!(&ops[..], [Op::Post { card: 1, .. }]), "{ops:?}");
    }

    #[test]
    fn a_busy_discord_is_waited_out_not_given_up() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "", "Hi", false));
        let [Op::Post { .. }] = &t.plan(0.0, 1.0, false)[..] else {
            panic!()
        };
        t.post_failed(Failure::Busy(5.0), 0.0);
        assert!(t.plan(1.0, 1.0, false).is_empty());
        assert_eq!(t.plan(5.0, 1.0, false).len(), 1);
        for _ in 0..10 {
            t.post_failed(Failure::Busy(1.0), 5.0);
        }
        assert_eq!(t.plan(9.0, 1.0, false).len(), 1, "never lost to a busy API");
    }

    #[test]
    fn a_deleted_newest_message_is_posted_again() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "", "Hi", false));
        land(&mut t, 0.0, false);
        t.absorb(&text(0, "Hi there\n", "", false));
        t.edit_failed(0, Failure::Gone, 1.0);
        let ops = land(&mut t, 1.0, false);
        assert!(matches!(&ops[..], [Op::Post { card: 0, .. }]), "{ops:?}");
    }

    #[test]
    fn a_forbidden_channel_ends_the_turns_messages() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "", "Hi", false));
        t.post_failed(Failure::Forbidden, 0.0);
        assert!(t.plan(99.0, 1.0, true).is_empty());
    }

    #[test]
    fn a_long_block_fills_messages_in_order_and_balances_fences() {
        let line = "x".repeat(99);
        let mut body = String::from("```rust\n");
        for _ in 0..50 {
            body.push_str(&line);
            body.push('\n');
        }
        body.push_str("```\nafter");
        let chunks = split_block(&body, CARD_TEXT);
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert!(chunks[0].ends_with("\n```"), "the cut closes its fence");
        assert!(chunks[1].starts_with("```rust\n"), "and reopens it");
        for chunk in &chunks {
            assert!(crate::text::utf16_len(chunk) <= CARD_TEXT);
        }
    }

    #[test]
    fn only_the_newest_message_carries_the_chip() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, &"word ".repeat(1000), "", false));
        let cards = t.render(0.0);
        assert_eq!(cards.len(), 2);
        for card in &cards {
            crate::render::validate_components(&card.components).unwrap();
        }
        assert!(
            cards[0].components.iter().all(|c| c["type"] == 10),
            "no chip: {:?}",
            cards[0].components
        );
        assert!(status_line(&cards[1]).starts_with("-# Working · started"));
    }

    #[test]
    fn a_runaway_block_is_capped() {
        let chunks = split_block(&"word ".repeat(20_000), CARD_TEXT);
        assert_eq!(chunks.len(), MAX_SPLIT);
        assert!(chunks.last().unwrap().ends_with("-# … (truncated)"));
    }

    #[test]
    fn a_turn_of_many_steps_stops_opening_messages() {
        let mut t = Timeline::new(true, 0.0);
        for step in 0..40u64 {
            t.absorb(&text(step, &format!("step {step}"), "", true));
            t.absorb(&ran(&format!("c{step}"), &format!("make {step}")));
        }
        let cards = t.render(0.0);
        assert_eq!(cards.len(), MAX_MESSAGES);
        let last = cards.last().unwrap();
        crate::render::validate_components(&last.components).unwrap();
        assert!(last.text.ends_with("-# Running `make 39`"), "{}", last.text);
    }

    #[test]
    fn a_shorter_answer_retires_extra_messages() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, &"y".repeat(5000), "", false));
        land(&mut t, 0.0, false);
        assert_eq!(t.cards.len(), 2);
        t.adopt("short");
        t.settle(Status::Done, 0.1);
        let ops = land(&mut t, 0.1, true);
        assert!(ops
            .iter()
            .any(|op| matches!(op, Op::Delete { card: 1, .. })));
        assert_eq!(t.landed(0.1).unwrap().parts.len(), 1);
    }

    #[test]
    fn streamed_media_tags_stay_out_of_the_preview() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "Chart:\nMEDIA:/tmp/a.png\n", "", false));
        assert_eq!(t.render(0.0)[0].text, "Chart: ▉");
    }
}
