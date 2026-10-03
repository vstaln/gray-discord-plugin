//! A turn as one live Components V2 card.
//!
//! With `GRAY_STREAM_TEXT=1`, gray's `--json` wire interleaves `text` rows
//! (the assistant's prose, numbered by segment, one segment per run of prose
//! between tool calls) with the tool progress rows. This module lays a turn
//! out as a Container that grows while the agent works:
//!
//! ```text
//! ┃ Let me check what's running on the box.          ← prose (Text Display)
//! ┃ -# Ran `gray ps` (0.3s)                           ← tool lines (subtext)
//! ┃ **Done / idle:** … ▉                             ← streaming prose
//! ┃ ───────────────────────────────────────────────  ← Separator
//! ┃ -# Working · started 20 seconds ago · ran 1 command [Stop] ← Section + Button
//! ```
//!
//! Prose and tool lines keep Hermes' order: each run of prose, then the tool
//! lines it led to, then the next prose. The accent bar tracks the turn's
//! status: grey while working, green when done, red on failure, dark grey when
//! stopped. The footer's clock is a Discord timestamp (`<t:…:R>`) that every
//! client keeps current on its own, so a quiet turn costs no edits.
//!
//! Once the turn settles the card gains what a finished turn needs: files
//! the answer named (`MEDIA:`) as a Media Gallery and File cards inside it,
//! and Retry / New chat buttons. A run of more than a handful of tool lines
//! folds to a one-line count plus its latest lines, with the full list in a
//! spoiler container below (tap to reveal). A long turn continues in further
//! cards (Discord caps one message at 40 components and 4000 characters);
//! only the last carries the footer.
//!
//! [`Timeline`] is the pure model. The gateway feeds it rows, asks it which
//! sends, edits and deletes are due ([`Timeline::plan`]), and reports back
//! what landed. Clock and transport stay outside so the layout is testable.

use serde_json::{json, Value};

/// Hermes' streaming cursor (`DEFAULT_STREAMING_CURSOR`).
pub const CURSOR: &str = " ▉";
/// Body text per card, in UTF-16 units. Discord allows 4000 across a V2
/// message; the rest is the footer's.
pub const CARD_TEXT: usize = 3600;
/// Text Displays per card. With the container, media, separator, footer,
/// buttons and tool log this stays under Discord's 40 components.
const MAX_PIECES: usize = 18;
/// A card with less room than this starts a fresh one rather than holding a
/// sliver of the next piece.
const MIN_ROOM: usize = 300;
/// Hermes' `MAX_SPLIT_MESSAGES`: a runaway turn never floods the channel.
pub const MAX_CARDS: usize = 8;
/// A tool group longer than this folds to a one-line count, with every
/// line in the tool log.
const FOLD_LINES: usize = 1;
/// Lines a folded group still shows while it is the live one, so the
/// action in flight stays visible.
const KEEP_LIVE: usize = 1;
/// The tool log's share of a card's text.
const LOG_CHARS: usize = 900;
/// Refusals before a turn's card is given up for good.
pub const MAX_REFUSALS: u32 = 3;
/// Discord's attachment cap per message.
pub const MAX_MEDIA: usize = 10;

/// gray's own grey while the turn runs, not Discord's blurple.
const WORKING: u32 = 0x99AAB5;
const DONE: u32 = 0x57F287;
const FAILED: u32 = 0xED4245;
/// Darker than the working grey, so a stopped turn still reads as ended.
const STOPPED: u32 = 0x4E5058;

/// Live replies on/off. Default on; `"stream_replies": false` keeps the
/// answer in one durable post at the end of the turn.
pub fn enabled(config: &Value) -> bool {
    config
        .get("stream_replies")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// Where the turn ended up; drives the accent and the footer.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Working,
    Done,
    /// A short reason: "failed", "timed out", "hit the budget".
    Failed(String),
    Stopped,
}

#[derive(Debug, Clone)]
enum Block {
    Text {
        segment: u64,
        /// Append-only lines from `delta`s.
        stable: String,
        /// The provisional unfinished line from the latest row.
        tail: String,
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
}

/// One card as it should look now.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub components: Vec<Value>,
    /// The card's body as plain markdown (no footer): what the durable
    /// outbox records for it, and what the activity hook reports.
    pub text: String,
    /// Uploads this card references as `attachment://<name>`; a send or
    /// edit must carry them.
    pub files: Vec<String>,
}

/// A file the finished answer named, shown inside the card.
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

/// The finished answer as it landed: each card's body and Discord id.
#[derive(Debug, Clone, PartialEq)]
pub struct Landed {
    pub parts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct Timeline {
    blocks: Vec<Block>,
    cards: Vec<Posted>,
    /// A card was refused for good (deleted, forbidden). The turn is left
    /// alone from then on rather than retried every frame.
    lost: bool,
    /// Sends and edits Discord refused this turn; see [`Timeline::refused`].
    refusals: u32,
    /// Stream prose for this turn. Off for slash-command turns (their
    /// answer belongs to the interaction) and when `stream_replies` is off;
    /// `text` rows are then ignored and the card shows tool lines only.
    text: bool,
    started: f64,
    /// How the turn ended, and when: the footer's time stops there.
    status: Option<(Status, f64)>,
    /// `custom_id` of the footer's Stop button while the turn runs.
    stop: Option<String>,
    /// `custom_id`s of the settled card's Retry and New chat buttons.
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

    /// Offer a Stop button in the footer while the turn runs.
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

    /// Show the answer's files inside the card (at most [`MAX_MEDIA`]).
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
    /// tags already taken out) to the prose it streamed as. Only when that
    /// prose is the last thing in the card: an answer above tool lines
    /// would read out of order, so the caller posts it fresh instead.
    pub fn adopt(&mut self, prose: &str) -> bool {
        if self.lost {
            return false;
        }
        let Some(Block::Text { done, answer, .. }) = self.blocks.last_mut() else {
            return false;
        };
        *done = true;
        *answer = Some(prose.to_string());
        true
    }

    /// Take the finished answer out of the card: the prose it streamed as
    /// is dropped when it is the last thing shown, so the caller can post
    /// the answer as its own message (a new message notifies with its
    /// text; an edit to a card never does). Earlier prose stays as
    /// narration.
    pub fn release_answer(&mut self) {
        if matches!(self.blocks.last(), Some(Block::Text { .. })) {
            self.blocks.pop();
        }
        self.close_text();
    }

    /// Record how the turn ended at `now`. The next plan settles the card.
    pub fn settle(&mut self, status: Status, now: f64) {
        self.status = Some((status, now));
    }

    /// The cards this turn should show at `now`.
    pub fn render(&self, now: f64) -> Vec<Card> {
        self.render_with(now, true)
    }

    fn render_with(&self, now: f64, buttons: bool) -> Vec<Card> {
        let (status, now) = self.status.clone().unwrap_or((Status::Working, now));
        let live = status == Status::Working;
        let mut pieces: Vec<String> = Vec::new();
        let mut tool_rows: Vec<Value> = Vec::new();
        let mut log: Vec<String> = Vec::new();
        let last_block = self.blocks.len().saturating_sub(1);
        for (index, block) in self.blocks.iter().enumerate() {
            match block {
                Block::Tools { rows } => {
                    tool_rows.extend(rows.iter().cloned());
                    if let Some(feed) = crate::activity::render(rows) {
                        let feed = crate::text::sanitize(&feed);
                        let lines: Vec<&str> = feed.lines().collect();
                        let shown: Vec<String> = if lines.len() > FOLD_LINES {
                            // Claude Code's "Ran 3 commands": one line per
                            // run of tools, the action in flight under it
                            // while it runs, every line in the tool log.
                            log.extend(lines.iter().map(|line| line.to_string()));
                            let summary = crate::activity::summary(rows).replace(" · ", ", ");
                            let mut shown = vec![if summary.is_empty() {
                                format!("{} tool calls", lines.len())
                            } else {
                                capitalized(&summary)
                            }];
                            if live && index == last_block {
                                shown.extend(
                                    lines[lines.len() - KEEP_LIVE..]
                                        .iter()
                                        .map(|line| line.to_string()),
                                );
                            }
                            shown
                        } else {
                            lines.iter().map(|line| line.to_string()).collect()
                        };
                        pieces.push(
                            shown
                                .iter()
                                .map(|line| format!("-# {line}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
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
                        continue;
                    }
                    let mut prose = prose.to_string();
                    if live && !done {
                        prose.push_str(CURSOR);
                    }
                    pieces.push(prose);
                }
            }
        }
        if pieces.is_empty() {
            return Vec::new();
        }
        let accent = match &status {
            Status::Working => WORKING,
            Status::Done => DONE,
            Status::Failed(_) => FAILED,
            Status::Stopped => STOPPED,
        };
        let footer = self.footer(&status, &crate::activity::summary(&tool_rows), now);
        let tool_log = tool_log(&log);
        let log_size = tool_log.as_ref().map_or(0, |(_, size)| *size);
        let bodies = pack(&pieces, CARD_TEXT.saturating_sub(log_size));
        let last = bodies.len() - 1;
        let settled = status != Status::Working;
        bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| {
                let mut children: Vec<Value> = body
                    .iter()
                    .map(|piece| json!({"type": 10, "content": piece}))
                    .collect();
                let mut files = Vec::new();
                let mut extra = Vec::new();
                if index == last {
                    children.extend(self.media_components());
                    files = self.media.iter().map(|media| media.name.clone()).collect();
                    children.push(json!({"type": 14, "divider": true, "spacing": 1}));
                    children.push(footer.clone());
                    if buttons && settled {
                        children.extend(self.action_row());
                    }
                    extra.extend(tool_log.clone().map(|(container, _)| container));
                }
                let mut components = vec![json!({
                    "type": 17,
                    "accent_color": accent,
                    "components": children,
                })];
                components.extend(extra);
                Card {
                    components,
                    text: body.join("\n"),
                    files,
                }
            })
            .collect()
    }

    /// The answer's files: images and videos in one gallery, the rest as
    /// File cards, all referencing uploads that travel with the edit.
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

    fn footer(&self, status: &Status, summary: &str, now: f64) -> Value {
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
        match (&self.stop, status) {
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
        }
    }

    /// The requests due now. Posts go strictly in card order; an edit waits
    /// `gap` seconds after the card's last change unless the turn is
    /// `finishing`, when everything settles at once.
    pub fn plan(&self, now: f64, gap: f64, finishing: bool) -> Vec<Op> {
        if self.lost {
            return Vec::new();
        }
        let cards = self.render(now);
        let mut ops = Vec::new();
        for (index, body) in cards.iter().enumerate() {
            match self.cards.get(index) {
                Some(posted) => {
                    if posted.shown != body.key() && (finishing || now - posted.at >= gap) {
                        ops.push(Op::Edit {
                            card: index,
                            id: posted.id.clone(),
                            body: body.clone(),
                        });
                    }
                }
                None => ops.push(Op::Post {
                    card: index,
                    body: body.clone(),
                }),
            }
        }
        // The turn shrank (the final answer is shorter than its preview):
        // retire the cards it no longer fills.
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
        if card == self.cards.len() {
            self.cards.push(Posted {
                id: id.to_string(),
                shown: body.key(),
                at: now,
            });
        }
    }

    /// An edit landed.
    pub fn edited(&mut self, card: usize, body: &Card, now: f64) {
        if let Some(posted) = self.cards.get_mut(card) {
            posted.shown = body.key();
            posted.at = now;
        }
    }

    /// A card was deleted (by us, at the end of the turn).
    pub fn deleted(&mut self, card: usize) {
        self.cards.truncate(card);
    }

    /// Discord refused a send or edit of `card`. The card and any after it
    /// are forgotten (left in the channel as they are) and the next plan
    /// posts them fresh, so one bad edit never freezes the turn. A turn
    /// refused [`MAX_REFUSALS`] times is given up.
    pub fn refused(&mut self, card: usize) {
        self.refusals += 1;
        if self.refusals >= MAX_REFUSALS {
            self.lost = true;
        } else {
            self.cards.truncate(card);
        }
    }

    /// Stop touching this turn.
    pub fn lose(&mut self) {
        self.lost = true;
    }

    /// Every card is on screen exactly as rendered at `now`.
    fn settled(&self, now: f64) -> Option<Vec<(String, String)>> {
        if self.lost {
            return None;
        }
        let cards = self.render(now);
        if cards.is_empty() || cards.len() != self.cards.len() {
            return None;
        }
        cards
            .into_iter()
            .zip(&self.cards)
            .map(|(body, posted)| {
                (posted.shown == body.key()).then(|| (body.text, posted.id.clone()))
            })
            .collect()
    }

    /// The adopted answer, if it is on screen exactly as rendered. `None`
    /// means the caller must deliver the answer itself.
    pub fn landed(&self, now: f64) -> Option<Landed> {
        if !matches!(
            self.blocks.last(),
            Some(Block::Text {
                answer: Some(_),
                ..
            })
        ) {
            return None;
        }
        self.settled(now).map(|parts| Landed { parts })
    }

    /// The card that shows how the turn ended, if it is on screen: a failed
    /// or stopped turn needs no separate notice when its footer says so.
    pub fn status_card(&self, now: f64) -> Option<String> {
        self.status.as_ref()?;
        self.settled(now)?.last().map(|(_, id)| id.clone())
    }

    /// The settled last card without its Retry / New chat buttons, for
    /// when the next turn starts: only the latest turn offers them. `None`
    /// when there is nothing to retire, or the card carries uploads (an
    /// edit would have to resend them).
    pub fn retired(&self) -> Option<(String, Vec<Value>)> {
        self.status.as_ref()?;
        if (self.retry.is_none() && self.new_chat.is_none()) || !self.media.is_empty() {
            return None;
        }
        self.settled(0.0)?;
        let cards = self.render_with(0.0, false);
        let last = cards.last()?;
        let posted = self.cards.get(cards.len() - 1)?;
        Some((posted.id.clone(), last.components.clone()))
    }

    /// Every card on screen, for retracting a preview whose answer did not
    /// land before the durable resend.
    pub fn posted_ids(&self) -> Vec<String> {
        self.cards.iter().map(|posted| posted.id.clone()).collect()
    }
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

/// The pressed card, flipped to "stopping…" for the interaction's own
/// UPDATE_MESSAGE response: grey accent, the Stop section replaced by a
/// line. The worker settles the card properly on its next frame. `None`
/// when the button is not on this card.
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

/// Fill cards in order. A piece that does not fit is cut to fill the room
/// left (Hermes seals an overflowing preview the same way), so content never
/// moves between cards as the turn grows: a cut lands on the last newline
/// that keeps at least half the room, and a code fence open at the cut is
/// closed there and reopened in the next card.
pub fn pack(pieces: &[String], budget: usize) -> Vec<Vec<String>> {
    let mut cards: Vec<Vec<String>> = vec![Vec::new()];
    let mut used = 0usize;
    for piece in pieces {
        let mut rest = piece.clone();
        while !rest.is_empty() {
            let room = budget.saturating_sub(used);
            let size = crate::text::utf16_len(&rest);
            let full = cards.last().map_or(0, Vec::len) >= MAX_PIECES;
            if full || (size > room && room < MIN_ROOM) {
                cards.push(Vec::new());
                used = 0;
                continue;
            }
            if size <= room {
                used += size;
                if let Some(card) = cards.last_mut() {
                    card.push(rest);
                }
                break;
            }
            let (head, tail) = split_at(&rest, room);
            if let Some(card) = cards.last_mut() {
                card.push(head);
            }
            cards.push(Vec::new());
            used = 0;
            rest = tail;
        }
    }
    cards.retain(|card| !card.is_empty());
    if cards.len() > MAX_CARDS {
        cards.truncate(MAX_CARDS);
        if let Some(card) = cards.last_mut() {
            card.push("-# … (truncated)".to_string());
        }
    }
    cards
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

    fn container(card: &Card) -> &Value {
        &card.components[0]
    }

    fn footer(card: &Card) -> String {
        let children = container(card)["components"].as_array().unwrap();
        let last = children.last().unwrap();
        match last["type"].as_u64() {
            Some(9) => last["components"][0]["content"]
                .as_str()
                .unwrap()
                .to_string(),
            _ => last["content"].as_str().unwrap().to_string(),
        }
    }

    #[test]
    fn a_turn_is_one_valid_v2_card_that_grows_in_place() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&text(0, "", "Hello", false));
        let ops = land(&mut t, 0.0, false);
        let [Op::Post { body, .. }] = &ops[..] else {
            panic!("{ops:?}")
        };
        crate::render::validate_components(&body.components).unwrap();
        assert_eq!(container(body)["accent_color"], json!(WORKING));
        assert_eq!(body.text, "Hello ▉");
        let stop = container(body)["components"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(stop["type"], 9, "footer is a Section with the Stop button");
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
    fn tool_lines_sit_between_prose_as_subtext() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "Let me check.", "", true));
        t.absorb(&ran("a", "cargo test"));
        t.absorb(&text(1, "", "All", false));
        let cards = t.render(0.0);
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].text,
            "Let me check.\n-# Running `cargo test`\nAll ▉"
        );
        assert_eq!(
            footer(&cards[0]),
            "-# Working · started <t:0:R> · ran 1 command"
        );
    }

    #[test]
    fn the_live_clock_never_costs_an_edit() {
        let mut t = Timeline::new(true, 1_700_000_000.0);
        t.absorb(&text(0, "", "Hi", false));
        land(&mut t, 1_700_000_000.0, false);
        assert_eq!(
            footer(&t.render(1_700_000_000.0)[0]),
            "-# Working · started <t:1700000000:R>",
            "Discord keeps the relative time current itself"
        );
        assert!(
            t.plan(1_700_000_600.0, 1.0, false).is_empty(),
            "ten idle minutes, no edits"
        );
    }

    #[test]
    fn a_tool_run_folds_to_one_line_like_claude_code() {
        let mut t = Timeline::new(true, 0.0);
        for i in 0..9 {
            t.absorb(&ran(&format!("c{i}"), &format!("step {i}")));
        }
        let card = &t.render(0.0)[0];
        crate::render::validate_components(&card.components).unwrap();
        // Live: the count, then the action in flight.
        assert_eq!(card.text, "-# Ran 9 commands\n-# Running `step 8`");
        // An earlier run, once prose follows it, is the count alone.
        t.absorb(&text(1, "Built it.\n", "", true));
        assert!(t.render(0.0)[0]
            .text
            .starts_with("-# Ran 9 commands\nBuilt it."));
        let log = &card.components[1];
        assert_eq!(log["type"], 17);
        assert_eq!(log["spoiler"], true, "tap to reveal");
        let lines = log["components"][1]["content"].as_str().unwrap();
        assert!(lines.starts_with("-# Running `step 0`"), "{lines}");
        assert_eq!(lines.lines().count(), 9);
    }

    #[test]
    fn settling_recolors_the_card_and_drops_the_stop_button() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&ran("a", "cargo test"));
        t.absorb(&text(0, "", "Draft", false));
        land(&mut t, 0.0, false);
        assert!(t.adopt("Final answer"));
        t.settle(Status::Done, 4.1);
        let ops = land(&mut t, 9.0, true);
        let [Op::Edit { body, .. }] = &ops[..] else {
            panic!("{ops:?}")
        };
        assert_eq!(container(body)["accent_color"], json!(DONE));
        assert_eq!(footer(body), "-# Done in 4.1s · ran 1 command");
        assert!(!body.key().contains("turn:stop"));
        let landed = t.landed(30.0).unwrap();
        assert_eq!(
            landed.parts,
            vec![(
                "-# Running `cargo test`\nFinal answer".to_string(),
                "m0".to_string()
            )]
        );
    }

    #[test]
    fn a_settled_card_offers_retry_and_new_chat_and_retires_them_later() {
        let mut t = Timeline::new(true, 0.0)
            .with_stop("turn:stop:s".into())
            .with_actions(Some("turn:retry:r".into()), Some("turn:new:n".into()));
        t.absorb(&text(0, "", "Hi", false));
        land(&mut t, 0.0, false);
        let live = t.render(0.0);
        assert!(!live[0].components[0].to_string().contains("turn:retry"));
        t.adopt("Hi there");
        t.settle(Status::Done, 2.0);
        land(&mut t, 2.0, true);
        let done = t.render(2.0);
        crate::render::validate_components(&done[0].components).unwrap();
        let children = container(&done[0])["components"].as_array().unwrap();
        let row = children.last().unwrap();
        assert_eq!(row["type"], 1, "an action row closes the settled card");
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
    fn the_answer_files_sit_inside_the_settled_card() {
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
        let children = container(body)["components"].as_array().unwrap();
        assert_eq!(children[1]["type"], 12, "gallery after the prose");
        assert_eq!(
            children[1]["items"][0]["media"]["url"],
            "attachment://chart.png"
        );
        assert_eq!(children[2]["type"], 13, "then the file card");
        assert!(t.landed(1.0).is_some());
        assert!(t.retired().is_none(), "a card with uploads is left as is");
    }

    #[test]
    fn stop_flips_the_pressed_card_at_once() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:s".into());
        t.absorb(&text(0, "", "Working", false));
        let mut echoed = json!(t.render(0.0)[0].components);
        // Discord echoes optional fields back as null.
        echoed[0]["components"][0]["id"] = Value::Null;
        let flipped = stopping(&echoed, "turn:stop:s").unwrap();
        crate::render::validate_components(&flipped).unwrap();
        assert_eq!(flipped[0]["accent_color"], json!(STOPPED));
        let text = flipped[0].to_string();
        assert!(text.contains("Stopping…") && !text.contains("turn:stop:s"));
        assert!(stopping(&echoed, "turn:stop:other").is_none());
    }

    #[test]
    fn a_stopped_turn_says_so_in_its_footer() {
        let mut t = Timeline::new(true, 0.0).with_stop("turn:stop:t".into());
        t.absorb(&text(0, "", "Working on", false));
        land(&mut t, 0.0, false);
        t.settle(Status::Stopped, 12.0);
        land(&mut t, 12.5, true);
        let card = &t.render(99.0)[0];
        assert_eq!(container(card)["accent_color"], json!(STOPPED));
        assert_eq!(
            footer(card),
            "-# Stopped after 12.0s · actions may already have happened"
        );
        assert_eq!(card.text, "Working on", "the cursor is gone");
        assert_eq!(t.status_card(99.0).as_deref(), Some("m0"));
    }

    #[test]
    fn an_answer_below_tool_lines_is_not_adopted() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, "Checking", "", true));
        t.absorb(&ran("a", "ls -la"));
        assert!(!t.adopt("Answer"));
        assert!(t.landed(0.0).is_none());
    }

    #[test]
    fn prose_rows_are_ignored_when_streaming_is_off() {
        let mut t = Timeline::new(false, 0.0);
        t.absorb(&text(0, "secret plan", "", true));
        assert!(!t.has_text());
        assert!(t.plan(0.0, 1.0, true).is_empty());
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
    fn a_long_turn_fills_cards_in_order_and_balances_fences() {
        let line = "x".repeat(99);
        let mut body = String::from("```rust\n");
        for _ in 0..50 {
            body.push_str(&line);
            body.push('\n');
        }
        body.push_str("```\nafter");
        let cards = pack(&["-# Ran `ls`".to_string(), body], CARD_TEXT);
        assert_eq!(cards.len(), 2, "{cards:?}");
        assert_eq!(cards[0][0], "-# Ran `ls`", "earlier content stays put");
        assert!(cards[0][1].ends_with("\n```"), "the cut closes its fence");
        assert!(cards[1][0].starts_with("```rust\n"), "and reopens it");
        for card in &cards {
            let size: usize = card.iter().map(|p| crate::text::utf16_len(p)).sum();
            assert!(size <= CARD_TEXT, "{size}");
        }
    }

    #[test]
    fn only_the_last_card_carries_the_footer() {
        let mut t = Timeline::new(true, 0.0);
        t.absorb(&text(0, &"word ".repeat(1000), "", false));
        let cards = t.render(0.0);
        assert_eq!(cards.len(), 2);
        for card in &cards {
            crate::render::validate_components(&card.components).unwrap();
        }
        let first = container(&cards[0])["components"].as_array().unwrap();
        assert!(
            first.iter().all(|c| c["type"] == 10),
            "no footer: {first:?}"
        );
        assert!(footer(&cards[1]).starts_with("-# Working · started"));
    }

    #[test]
    fn a_runaway_turn_is_capped() {
        let cards = pack(&["word ".repeat(20_000)], CARD_TEXT);
        assert_eq!(cards.len(), MAX_CARDS);
        assert_eq!(cards.last().unwrap().last().unwrap(), "-# … (truncated)");
    }

    #[test]
    fn a_shorter_answer_retires_extra_cards() {
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
