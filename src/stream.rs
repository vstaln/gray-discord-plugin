//! A turn as one live Components V2 card.
//!
//! With `GRAY_STREAM_TEXT=1`, gray's `--json` wire interleaves `text` rows
//! (the assistant's prose, numbered by segment, one segment per run of prose
//! between tool calls) with the tool progress rows. This module lays a turn
//! out as a Container that grows while the agent works:
//!
//! ```text
//! ┃ Let me check what's running on the box.          ← prose (Text Display)
//! ┃ -# 💻 Ran `gray ps` (0.3s)                        ← tool lines (subtext)
//! ┃ **Done / idle:** … ▉                             ← streaming prose
//! ┃ ───────────────────────────────────────────────  ← Separator
//! ┃ -# ⏳ working · 20s · ran 1 command     [⏹️ Stop] ← Section + Button
//! ```
//!
//! Prose and tool lines keep Hermes' order: each run of prose, then the tool
//! lines it led to, then the next prose. The accent bar tracks the turn's
//! status: blurple while working, green when done, red on failure, grey when
//! stopped. A long turn continues in further cards (Discord caps one message
//! at 40 components and 4000 characters); only the last carries the footer.
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
/// Text Displays per card: with the container, separator and footer this
/// stays well under Discord's 40 components.
const MAX_PIECES: usize = 30;
/// A card with less room than this starts a fresh one rather than holding a
/// sliver of the next piece.
const MIN_ROOM: usize = 300;
/// Hermes' `MAX_SPLIT_MESSAGES`: a runaway turn never floods the channel.
pub const MAX_CARDS: usize = 8;
/// The footer's elapsed time moves in steps this long (seconds), so an idle
/// turn costs one edit per step instead of one per second.
const CLOCK_STEP: f64 = 10.0;

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
    /// Stream prose for this turn. Off for slash-command turns (their
    /// answer belongs to the interaction) and when `stream_replies` is off;
    /// `text` rows are then ignored and the card shows tool lines only.
    text: bool,
    started: f64,
    /// How the turn ended, and when: the footer's time stops there.
    status: Option<(Status, f64)>,
    /// `custom_id` of the footer's Stop button while the turn runs.
    stop: Option<String>,
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

    /// Record how the turn ended at `now`. The next plan settles the card.
    pub fn settle(&mut self, status: Status, now: f64) {
        self.status = Some((status, now));
    }

    /// The cards this turn should show at `now`.
    pub fn render(&self, now: f64) -> Vec<Card> {
        let (status, now) = self.status.clone().unwrap_or((Status::Working, now));
        let live = status == Status::Working;
        let mut pieces: Vec<String> = Vec::new();
        let mut tool_rows: Vec<Value> = Vec::new();
        for block in &self.blocks {
            match block {
                Block::Tools { rows } => {
                    tool_rows.extend(rows.iter().cloned());
                    if let Some(feed) = crate::activity::render(rows) {
                        let feed = crate::text::sanitize(&feed);
                        pieces.push(
                            feed.lines()
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
        let bodies = pack(&pieces);
        let last = bodies.len() - 1;
        bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| {
                let mut children: Vec<Value> = body
                    .iter()
                    .map(|piece| json!({"type": 10, "content": piece}))
                    .collect();
                if index == last {
                    children.push(json!({"type": 14, "divider": true, "spacing": 1}));
                    children.push(footer.clone());
                }
                Card {
                    components: vec![json!({
                        "type": 17,
                        "accent_color": accent,
                        "components": children,
                    })],
                    text: body.join("\n"),
                }
            })
            .collect()
    }

    fn footer(&self, status: &Status, summary: &str, now: f64) -> Value {
        let elapsed = (now - self.started).max(0.0);
        let mut line = match status {
            Status::Working if elapsed >= CLOCK_STEP => {
                let stepped = (elapsed / CLOCK_STEP).floor() * CLOCK_STEP;
                format!("⏳ working · {}", duration(stepped, false))
            }
            Status::Working => "⏳ working".to_string(),
            Status::Done => format!("✅ done in {}", duration(elapsed, true)),
            Status::Failed(reason) => format!("❌ {reason} after {}", duration(elapsed, true)),
            Status::Stopped => format!("⏹️ stopped after {}", duration(elapsed, true)),
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
                    "emoji": {"name": "⏹️"},
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

    /// A send or edit was refused: stop touching this turn.
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

    /// Every card on screen, for retracting a preview whose answer did not
    /// land before the durable resend.
    pub fn posted_ids(&self) -> Vec<String> {
        self.cards.iter().map(|posted| posted.id.clone()).collect()
    }
}

/// `4.1s`, `1m 12s`; `round` keeps tenths under a minute.
fn duration(secs: f64, round: bool) -> String {
    if secs < 60.0 {
        return if round {
            format!("{secs:.1}s")
        } else {
            format!("{}s", secs as u64)
        };
    }
    let whole = secs as u64;
    format!("{}m {:02}s", whole / 60, whole % 60)
}

/// Fill cards in order. A piece that does not fit is cut to fill the room
/// left (Hermes seals an overflowing preview the same way), so content never
/// moves between cards as the turn grows: a cut lands on the last newline
/// that keeps at least half the room, and a code fence open at the cut is
/// closed there and reopened in the next card.
pub fn pack(pieces: &[String]) -> Vec<Vec<String>> {
    let mut cards: Vec<Vec<String>> = vec![Vec::new()];
    let mut used = 0usize;
    for piece in pieces {
        let mut rest = piece.clone();
        while !rest.is_empty() {
            let room = CARD_TEXT.saturating_sub(used);
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
            "Let me check.\n-# 💻 Running `cargo test`\nAll ▉"
        );
        assert_eq!(footer(&cards[0]), "-# ⏳ working · ran 1 command");
    }

    #[test]
    fn the_footer_clock_moves_in_steps() {
        let mut t = Timeline::new(true, 100.0);
        t.absorb(&text(0, "", "Hi", false));
        assert_eq!(footer(&t.render(105.0)[0]), "-# ⏳ working");
        assert_eq!(footer(&t.render(127.0)[0]), "-# ⏳ working · 20s");
        assert_eq!(footer(&t.render(195.0)[0]), "-# ⏳ working · 1m 30s");
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
        assert_eq!(footer(body), "-# ✅ done in 4.1s · ran 1 command");
        assert!(!body.key().contains("turn:stop"));
        let landed = t.landed(30.0).unwrap();
        assert_eq!(
            landed.parts,
            vec![(
                "-# 💻 Running `cargo test`\nFinal answer".to_string(),
                "m0".to_string()
            )]
        );
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
            "-# ⏹️ stopped after 12.0s · actions may already have happened"
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
        let cards = pack(&["-# 💻 Ran `ls`".to_string(), body]);
        assert_eq!(cards.len(), 2, "{cards:?}");
        assert_eq!(cards[0][0], "-# 💻 Ran `ls`", "earlier content stays put");
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
        assert!(footer(&cards[1]).starts_with("-# ⏳"));
    }

    #[test]
    fn a_runaway_turn_is_capped() {
        let cards = pack(&["word ".repeat(20_000)]);
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
