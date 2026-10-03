//! Hermes-style live reply: one turn laid out as an ordered run of messages.
//!
//! With `GRAY_STREAM_TEXT=1`, gray's `--json` wire interleaves `text` rows
//! (the assistant's prose, numbered by segment, one segment per run of prose
//! between tool calls) with the tool progress rows. Hermes renders that as a
//! timeline, and so does this module:
//!
//! - each prose segment is its own message, edited in place while it
//!   streams, with a ` ▉` cursor until it is done;
//! - tool lines collect in one plain bubble below the prose that preceded
//!   them, and the next prose starts a new message below that bubble;
//! - everything is a Components V2 Text Display: no embed-style cards.
//!
//! [`Timeline`] is the pure model. The gateway feeds it rows, asks it which
//! sends, edits and deletes are due ([`Timeline::plan`]), and reports back
//! what landed. Clock and transport stay outside so the layout is testable.

use serde_json::Value;

/// Hermes' streaming cursor (`DEFAULT_STREAMING_CURSOR`).
pub const CURSOR: &str = " ▉";
/// One message's share of a long body, in UTF-16 units: Discord's 2000 with
/// room for the cursor and a closing code fence.
pub const CHUNK_LIMIT: usize = 1990;
/// Hermes' `MAX_SPLIT_MESSAGES`: a runaway body never floods the channel.
pub const MAX_CHUNKS: usize = 8;

/// Live replies on/off. Default on; `"stream_replies": false` keeps the
/// answer in one durable post at the end of the turn.
pub fn enabled(config: &Value) -> bool {
    config
        .get("stream_replies")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

#[derive(Debug, Clone)]
enum Body {
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

#[derive(Debug, Clone)]
struct Block {
    body: Body,
    messages: Vec<Posted>,
    /// A send or edit failed for good (deleted, forbidden). The block is
    /// left alone from then on rather than retried every frame.
    lost: bool,
}

/// One request the gateway should make, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Post {
        block: usize,
        chunk: usize,
        body: String,
    },
    Edit {
        block: usize,
        chunk: usize,
        id: String,
        body: String,
    },
    Delete {
        block: usize,
        chunk: usize,
        id: String,
    },
}

/// The final answer as it landed: each message's body and Discord id.
#[derive(Debug, Clone, PartialEq)]
pub struct Landed {
    pub parts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct Timeline {
    blocks: Vec<Block>,
    /// Stream prose for this turn. Off for slash-command turns (their
    /// answer belongs to the interaction) and when `stream_replies` is off;
    /// `text` rows are then ignored and only the tool bubble runs.
    text: bool,
}

impl Timeline {
    pub fn new(text: bool) -> Self {
        Self {
            blocks: Vec::new(),
            text,
        }
    }

    /// Fold one progress row into the layout.
    pub fn absorb(&mut self, row: &Value) {
        if row.get("phase").and_then(Value::as_str) == Some("text") {
            if self.text {
                self.absorb_text(row);
            }
            return;
        }
        if let Some(Block {
            body: Body::Tools { rows },
            ..
        }) = self.blocks.last_mut()
        {
            rows.push(row.clone());
            return;
        }
        if !crate::activity::narrates(row) {
            return;
        }
        // A tool after prose closes it, even if gray's closing row was lost.
        self.close_text();
        self.blocks.push(Block::new(Body::Tools {
            rows: vec![row.clone()],
        }));
    }

    fn absorb_text(&mut self, row: &Value) {
        let segment = row.get("segment").and_then(Value::as_u64).unwrap_or(0);
        let delta = row.get("delta").and_then(Value::as_str).unwrap_or("");
        let new_tail = row.get("tail").and_then(Value::as_str).unwrap_or("");
        let closes = row.get("done").and_then(Value::as_bool).unwrap_or(false);
        let index = self.blocks.iter().rposition(
            |block| matches!(&block.body, Body::Text { segment: s, .. } if *s == segment),
        );
        let index = match index {
            Some(index) => index,
            None => {
                self.close_text();
                self.blocks.push(Block::new(Body::Text {
                    segment,
                    stable: String::new(),
                    tail: String::new(),
                    done: false,
                    answer: None,
                }));
                self.blocks.len() - 1
            }
        };
        if let Body::Text {
            stable, tail, done, ..
        } = &mut self.blocks[index].body
        {
            stable.push_str(delta);
            *tail = new_tail.to_string();
            *done |= closes;
        }
    }

    fn close_text(&mut self) {
        for block in &mut self.blocks {
            if let Body::Text { done, .. } = &mut block.body {
                *done = true;
            }
        }
    }

    /// Whether any prose has been streamed this turn.
    pub fn has_text(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block.body, Body::Text { .. }))
    }

    /// Hand the finished turn's authoritative answer (prose only, `MEDIA:`
    /// tags already taken out) to the prose it streamed as. Only when that
    /// prose is the last thing on screen: an answer above a tool bubble
    /// would read out of order, so the caller posts it fresh instead.
    pub fn adopt(&mut self, prose: &str) -> bool {
        let Some(Block {
            body: Body::Text { done, answer, .. },
            lost: false,
            ..
        }) = self.blocks.last_mut()
        else {
            return false;
        };
        *done = true;
        *answer = Some(prose.to_string());
        true
    }

    /// The requests due now. Posts go strictly in timeline order; an edit
    /// waits `gap` seconds after the message's last change unless the turn
    /// is `finishing`, when everything settles at once.
    pub fn plan(&self, now: f64, gap: f64, finishing: bool) -> Vec<Op> {
        let mut ops = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if block.lost {
                continue;
            }
            let chunks = block.render(finishing);
            for (chunk, body) in chunks.iter().enumerate() {
                match block.messages.get(chunk) {
                    Some(posted) => {
                        if posted.shown != *body && (finishing || now - posted.at >= gap) {
                            ops.push(Op::Edit {
                                block: index,
                                chunk,
                                id: posted.id.clone(),
                                body: body.clone(),
                            });
                        }
                    }
                    None => ops.push(Op::Post {
                        block: index,
                        chunk,
                        body: body.clone(),
                    }),
                }
            }
            // The body shrank (the final answer is shorter than its
            // preview): retire the messages it no longer fills.
            if finishing {
                for (chunk, posted) in block.messages.iter().enumerate().skip(chunks.len()) {
                    ops.push(Op::Delete {
                        block: index,
                        chunk,
                        id: posted.id.clone(),
                    });
                }
            }
        }
        ops
    }

    /// A post landed as message `id`.
    pub fn posted(&mut self, block: usize, chunk: usize, id: &str, body: &str, now: f64) {
        if let Some(block) = self.blocks.get_mut(block) {
            if chunk == block.messages.len() {
                block.messages.push(Posted {
                    id: id.to_string(),
                    shown: body.to_string(),
                    at: now,
                });
            }
        }
    }

    /// An edit landed.
    pub fn edited(&mut self, block: usize, chunk: usize, body: &str, now: f64) {
        if let Some(posted) = self
            .blocks
            .get_mut(block)
            .and_then(|block| block.messages.get_mut(chunk))
        {
            posted.shown = body.to_string();
            posted.at = now;
        }
    }

    /// A message was deleted (by us, at the end of the turn).
    pub fn deleted(&mut self, block: usize, chunk: usize) {
        if let Some(block) = self.blocks.get_mut(block) {
            block.messages.truncate(chunk);
        }
    }

    /// A send or edit failed for good: stop touching this block.
    pub fn lose(&mut self, block: usize) {
        if let Some(block) = self.blocks.get_mut(block) {
            block.lost = true;
        }
    }

    /// The adopted answer, if every message of it is on screen exactly as
    /// rendered. `None` means the caller must deliver the answer itself.
    pub fn landed(&self) -> Option<Landed> {
        let block = self.blocks.last()?;
        if block.lost
            || !matches!(
                &block.body,
                Body::Text {
                    answer: Some(_),
                    ..
                }
            )
        {
            return None;
        }
        let chunks = block.render(true);
        if chunks.len() != block.messages.len() {
            return None;
        }
        let mut parts = Vec::with_capacity(chunks.len());
        for (body, posted) in chunks.into_iter().zip(&block.messages) {
            if posted.shown != body {
                return None;
            }
            parts.push((body, posted.id.clone()));
        }
        Some(Landed { parts })
    }

    /// Messages showing a preview of an answer that did not land, so the
    /// caller can retract them before the durable resend.
    pub fn stale_answer(&self) -> Vec<String> {
        match self.blocks.last() {
            Some(block)
                if matches!(
                    &block.body,
                    Body::Text {
                        answer: Some(_),
                        ..
                    }
                ) =>
            {
                block
                    .messages
                    .iter()
                    .map(|posted| posted.id.clone())
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

impl Block {
    fn new(body: Body) -> Self {
        Self {
            body,
            messages: Vec::new(),
            lost: false,
        }
    }

    /// The bodies this block should show, one per message.
    fn render(&self, finishing: bool) -> Vec<String> {
        match &self.body {
            Body::Tools { rows } => crate::activity::render(rows)
                .map(|feed| chunks(&crate::text::sanitize(&feed)))
                .unwrap_or_default(),
            Body::Text {
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
                    return Vec::new();
                }
                let mut out = chunks(prose);
                if !done && !finishing {
                    if let Some(last) = out.last_mut() {
                        last.push_str(CURSOR);
                    }
                }
                out
            }
        }
    }
}

/// Split a body into message-sized chunks the way Hermes seals an
/// overflowing preview: cut at the last newline in the budget when that
/// keeps at least half of it, and close a code fence a cut lands inside
/// (reopening it at the top of the next chunk). The cut points depend only
/// on the text before them, so a growing body never reshuffles what
/// earlier messages already show.
pub fn chunks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut reopen = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let whole = format!("{reopen}{rest}");
        if crate::text::utf16_len(&whole) <= CHUNK_LIMIT {
            out.push(whole);
            break;
        }
        if out.len() + 1 == MAX_CHUNKS {
            let budget = CHUNK_LIMIT.saturating_sub(crate::text::utf16_len(&reopen) + 32);
            let head = crate::text::prefix_within_limit(rest, budget);
            let mut last = close_fence(&format!("{reopen}{head}")).0;
            last.push_str("\n… (truncated)");
            out.push(last);
            break;
        }
        // Room for the reopened fence above and a closing fence below.
        let budget = CHUNK_LIMIT.saturating_sub(crate::text::utf16_len(&reopen) + 4);
        let head = crate::text::prefix_within_limit(rest, budget);
        let cut = match head.rfind('\n') {
            Some(at) if at >= head.len() / 2 => at,
            _ => head.len(),
        };
        let (closed, open) = close_fence(&format!("{reopen}{}", &rest[..cut]));
        out.push(closed);
        reopen = open.map(|fence| format!("{fence}\n")).unwrap_or_default();
        rest = rest[cut..].trim_start_matches('\n');
    }
    out
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
    use serde_json::json;

    fn text(segment: u64, delta: &str, tail: &str, done: bool) -> Value {
        json!({"phase": "text", "segment": segment, "delta": delta, "tail": tail, "done": done})
    }

    fn ran(id: &str, command: &str) -> Value {
        json!({"phase": "tool_ran", "call_id": id, "tool": "bash", "detail": command})
    }

    /// Apply every op as if Discord accepted it.
    fn land(timeline: &mut Timeline, now: f64, finishing: bool) -> Vec<Op> {
        let ops = timeline.plan(now, 1.0, finishing);
        let mut next = 100 + timeline.blocks.len() * 10;
        for op in &ops {
            match op {
                Op::Post { block, chunk, body } => {
                    next += 1;
                    timeline.posted(*block, *chunk, &next.to_string(), body, now);
                }
                Op::Edit {
                    block, chunk, body, ..
                } => timeline.edited(*block, *chunk, body, now),
                Op::Delete { block, chunk, .. } => timeline.deleted(*block, *chunk),
            }
        }
        ops
    }

    #[test]
    fn prose_streams_into_one_message_with_a_cursor() {
        let mut t = Timeline::new(true);
        t.absorb(&text(0, "", "Hello", false));
        let ops = land(&mut t, 0.0, false);
        assert_eq!(
            ops,
            vec![Op::Post {
                block: 0,
                chunk: 0,
                body: "Hello ▉".into()
            }]
        );
        t.absorb(&text(0, "Hello world\n", "and", false));
        assert!(t.plan(0.5, 1.0, false).is_empty(), "inside the edit gap");
        let ops = land(&mut t, 1.0, false);
        assert!(
            matches!(&ops[..], [Op::Edit { body, .. }] if body == "Hello world\nand ▉"),
            "{ops:?}"
        );
    }

    #[test]
    fn tools_sit_between_prose_segments_in_order() {
        let mut t = Timeline::new(true);
        t.absorb(&text(0, "Let me check.", "", true));
        t.absorb(&ran("a", "cargo test"));
        t.absorb(&text(1, "", "All", false));
        let ops = land(&mut t, 0.0, false);
        let bodies: Vec<&str> = ops
            .iter()
            .map(|op| match op {
                Op::Post { body, .. } => body.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(
            bodies,
            vec!["Let me check.", "💻 Running `cargo test`", "All ▉"]
        );
    }

    #[test]
    fn the_answer_replaces_its_preview_and_lands() {
        let mut t = Timeline::new(true);
        t.absorb(&text(0, "", "Draft", false));
        land(&mut t, 0.0, false);
        assert!(t.adopt("Final answer"));
        land(&mut t, 0.1, true);
        let landed = t.landed().unwrap();
        assert_eq!(landed.parts, vec![("Final answer".into(), "111".into())]);
    }

    #[test]
    fn an_answer_below_a_tool_bubble_is_not_adopted() {
        let mut t = Timeline::new(true);
        t.absorb(&text(0, "Checking", "", true));
        t.absorb(&ran("a", "ls -la"));
        assert!(!t.adopt("Answer"));
        assert!(t.landed().is_none());
    }

    #[test]
    fn prose_rows_are_ignored_when_streaming_is_off() {
        let mut t = Timeline::new(false);
        t.absorb(&text(0, "secret plan", "", true));
        assert!(!t.has_text());
        assert!(t.plan(0.0, 1.0, true).is_empty());
    }

    #[test]
    fn a_failed_post_loses_the_block_and_the_answer_is_not_landed() {
        let mut t = Timeline::new(true);
        t.absorb(&text(0, "", "Hi", false));
        t.lose(0);
        assert!(!t.adopt("Hi there"), "a lost block cannot carry the answer");
        assert!(t.landed().is_none());
    }

    #[test]
    fn long_prose_seals_earlier_messages_and_balances_fences() {
        let line = "x".repeat(99);
        let mut body = String::from("```rust\n");
        for _ in 0..30 {
            body.push_str(&line);
            body.push('\n');
        }
        body.push_str("```\nafter");
        let parts = chunks(&body);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].ends_with("\n```"), "first chunk closes its fence");
        assert!(parts[1].starts_with("```rust\n"), "second reopens it");
        for part in &parts {
            assert!(crate::text::utf16_len(part) <= CHUNK_LIMIT);
        }
    }

    #[test]
    fn a_runaway_body_is_capped() {
        let body = "word ".repeat(10_000);
        let parts = chunks(&body);
        assert_eq!(parts.len(), MAX_CHUNKS);
        assert!(parts.last().unwrap().ends_with("… (truncated)"));
    }

    #[test]
    fn a_shorter_answer_retires_extra_preview_messages() {
        let mut t = Timeline::new(true);
        let long = "y".repeat(3000);
        t.absorb(&text(0, &long, "", false));
        land(&mut t, 0.0, false);
        assert_eq!(t.blocks[0].messages.len(), 2);
        t.adopt("short");
        let ops = land(&mut t, 0.1, true);
        assert!(ops
            .iter()
            .any(|op| matches!(op, Op::Delete { chunk: 1, .. })));
        assert_eq!(t.landed().unwrap().parts.len(), 1);
    }

    #[test]
    fn streamed_media_tags_stay_out_of_the_preview() {
        let mut t = Timeline::new(true);
        t.absorb(&text(0, "Chart:\nMEDIA:/tmp/a.png\n", "", false));
        let ops = land(&mut t, 0.0, false);
        assert!(
            matches!(&ops[..], [Op::Post { body, .. }] if body == "Chart: ▉"),
            "{ops:?}"
        );
    }
}
