//! Turn narration for chat surfaces.
//!
//! gray's `--json` progress rows are platform-agnostic: a tool name, a
//! redacted one-line detail, and (for completed tools) a bounded redacted
//! output. This module is the shared plumbing between the runner (which reads
//! those rows) and the gateway (which knows the channel): a bounded, scoped
//! sink plus pure renderers for the live status bubble and the persistent
//! tool-activity card.
//!
//! Both renderers follow Hermes' tool-progress feed: one line per call —
//! icon, tool, a bounded preview of the arguments, how long it took — and
//! **never the tool's output**. A chat surface is a status feed, not a
//! transcript; echoing results dumps file contents, memory snapshots and
//! credentials into the channel.
//!
//! The sink also carries gray's streamed `text` rows, in order with the tool
//! rows, so [`crate::stream`] can lay the turn out as Hermes does. Text rows
//! stay out of the bounded history: they would crowd the tool rows the
//! optional end-of-turn card is built from.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

/// Bounded so a runaway turn cannot grow the queue without limit. 64 pending
/// rows is far more than one status bubble shows; the oldest fall off.
const MAX_PENDING: usize = 64;
/// Keep a bounded history for the final card even after the live rows have
/// been drained. This is intentionally larger than the pending queue so a
/// turn with many quick tools still produces a useful transcript.
const MAX_HISTORY: usize = 128;
/// The card lists actions, not transcripts: a handful of rows plus a count.
const MAX_CARD_ROWS: usize = 5;
/// Discord rejects a Text Display past 4000 chars (see render.rs); the card
/// stays well under.
const MAX_CARD_CHARS: usize = 3600;

/// Hermes' suggested cap for a tool preview line
/// (`display.tool_preview_length: 80`).
const MAX_PREVIEW_CHARS: usize = 80;

/// Seconds between edits of the same bubble (Discord rate-limits edits).
pub const MIN_EDIT_GAP: f64 = 1.0;

#[derive(Default)]
struct ScopeState {
    pending: VecDeque<Value>,
    history: VecDeque<Value>,
    /// When this turn started, for the card's elapsed tally. Set by `begin`.
    started: Option<Instant>,
    /// In-flight internal call ids -> when they started, so a line can carry
    /// the call's duration once it returns. gray's rows carry no clock.
    inflight: HashMap<String, Instant>,
}

#[derive(Default)]
pub struct SinkState {
    scopes: HashMap<String, ScopeState>,
}

pub type Sink = Arc<Mutex<SinkState>>;

pub fn sink() -> Sink {
    Arc::new(Mutex::new(SinkState::default()))
}

/// Narration on/off. Default on, same semantics as `typing_indicator`:
/// absent means enabled, only an explicit `false` turns it off.
pub fn enabled(config: &Value) -> bool {
    config
        .get("activity_indicator")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// The end-of-turn tally card (`⋯ 12.4s · ran 3 commands`). Off by default:
/// Hermes posts none, and the progress bubbles already stay in the channel
/// as the turn's record. `"activity_card": true` brings it back.
pub fn card_enabled(config: &Value) -> bool {
    config
        .get("activity_card")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Start a fresh history for one conversation. The daemon may have a stale
/// entry after a crash or a cancelled turn; never let it bleed into the next
/// turn's card.
pub fn begin(s: &Sink, scope: &str) {
    if let Ok(mut state) = s.lock() {
        state.scopes.insert(
            scope.to_string(),
            ScopeState {
                started: Some(Instant::now()),
                ..ScopeState::default()
            },
        );
    }
}

pub fn push(s: &Sink, row: Value) {
    push_for(s, "", row);
}

pub fn push_for(s: &Sink, scope: &str, row: Value) {
    let Ok(mut state) = s.lock() else {
        return;
    };
    let scope = state.scopes.entry(scope.to_string()).or_default();
    let mut row = row;
    stamp(&mut row, scope);
    if scope.pending.len() >= MAX_PENDING {
        scope.pending.pop_front();
    }
    scope.pending.push_back(row.clone());
    if row.get("phase").and_then(Value::as_str) == Some("text") {
        return;
    }
    if scope.history.len() >= MAX_HISTORY {
        scope.history.pop_front();
    }
    scope.history.push_back(row);
}

/// Hermes parity: a narrated line carries how long the call took and the
/// card's tally carries how long the turn took. gray's rows have no clock,
/// so the sink measures between `tool_ran` and `tool_finished` for the same
/// internal call id.
fn stamp(row: &mut Value, scope: &mut ScopeState) {
    let phase = row.get("phase").and_then(Value::as_str).unwrap_or("");
    let call_id = row
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match phase {
        "tool_ran" => {
            if !call_id.is_empty() {
                scope.inflight.insert(call_id, Instant::now());
            }
        }
        "tool_finished" => {
            if !call_id.is_empty() {
                if let Some(started) = scope.inflight.remove(&call_id) {
                    row["elapsed_ms"] = millis(started.elapsed()).into();
                }
            }
            if let Some(turn) = scope.started {
                row["turn_ms"] = millis(turn.elapsed()).into();
            }
        }
        _ => {}
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn drain(s: &Sink) -> Vec<Value> {
    drain_for(s, "")
}

pub fn drain_for(s: &Sink, scope: &str) -> Vec<Value> {
    let mut state = match s.lock() {
        Ok(state) => state,
        Err(_) => return Vec::new(),
    };
    state
        .scopes
        .get_mut(scope)
        .map(|scope| scope.pending.drain(..).collect())
        .unwrap_or_default()
}

/// Remove a completed turn and return its full bounded history. Removing the
/// scope makes final-card publication idempotent even if a caller flushes
/// twice.
pub fn finish(s: &Sink, scope: &str) -> Vec<Value> {
    let mut state = match s.lock() {
        Ok(state) => state,
        Err(_) => return Vec::new(),
    };
    state
        .scopes
        .remove(scope)
        .map(|scope| scope.history.into_iter().collect())
        .unwrap_or_default()
}

pub fn discard(s: &Sink, scope: &str) {
    if let Ok(mut state) = s.lock() {
        state.scopes.remove(scope);
    }
}

/// The runner's progress callback: push, never block, never fail a turn.
pub fn callback<'a>(s: Sink) -> crate::runner::ProgressFn<'a> {
    callback_for(s, String::new())
}

/// Scoped callback used by the daemon so concurrent channels cannot drain or
/// finalize one another's rows.
pub fn callback_for<'a>(s: Sink, scope: String) -> crate::runner::ProgressFn<'a> {
    Box::new(move |row: &Value| push_for(&s, &scope, row.clone()))
}

/// Tools whose work is nobody's status: they are the bulk of a turn (read and
/// search loops) and mean nothing to a reader. The card's tally still counts
/// them.
fn is_quiet(tool: &str) -> bool {
    matches!(tool, "read" | "cat" | "find" | "grep" | "glob" | "ls")
}

/// Same test for a full progress row: `ls` through the shell is the same
/// nobody-status as the `ls` tool, so a `gray view` turn never also narrates
/// its `ls` neighbor. Counts are untouched — the tally still counts these.
fn is_quiet_row(tool: &str, detail: &str) -> bool {
    if is_quiet(tool) {
        return true;
    }
    if matches!(tool, "bash" | "shell") {
        let first = clean_command(detail)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        return matches!(
            first.as_str(),
            "ls" | "cat" | "find" | "grep" | "glob" | "read"
        );
    }
    false
}

/// The tool's icon plus its short name. Shell work reads the way gray's own
/// transcript labels it — "Running" while live, "Ran" once done — never
/// "terminal".
fn label(tool: &str) -> String {
    match tool {
        "bash" | "shell" => "💻 Running".to_string(),
        "read" | "cat" => "📖 read".to_string(),
        "write" | "create" | "str_replace" => "✍️ write".to_string(),
        "edit" | "apply_patch" => "✍️ edit".to_string(),
        "" => "🔧 working".to_string(),
        other => format!("🔧 {other}"),
    }
}

/// One line for one row, or `None` when the row adds nothing a reader
/// needs. A finished tool with no error is silence — the `ran` line above
/// it already said what happened.
fn line(row: &Value, elapsed: Option<u64>) -> Option<String> {
    let phase = row.get("phase").and_then(Value::as_str).unwrap_or("");
    let tool = row.get("tool").and_then(Value::as_str).unwrap_or("");
    let detail = row.get("detail").and_then(Value::as_str).unwrap_or("");
    match phase {
        // The call has begun but has not reported its arguments yet. The
        // matching `tool_ran` overwrites this line rather than stacking a
        // second one for the same call.
        "tool_started" => Some(label(tool)),
        "tool_ran" => Some(ran_line(tool, detail, elapsed)),
        "tool_finished" if row.get("error").and_then(Value::as_bool) == Some(true) => Some(
            format!("❌ {} failed", if tool.is_empty() { "tool" } else { tool }),
        ),
        "provider_retry" if !detail.is_empty() => Some(format!("⚠️ {}", one_line(detail))),
        "compacted" => Some("🗜 context compacted".to_string()),
        _ => None,
    }
}

/// A call reads "Running" while it runs and "Ran" once it has returned —
/// the feed stays in the channel after the turn, so it must read as done.
fn ran_line(tool: &str, detail: &str, elapsed: Option<u64>) -> String {
    shell_line(tool, detail, elapsed, elapsed.is_some())
}

/// Whether `row` puts a line in the feed. A row that does not (a quiet
/// finish, a phase marker) never opens a new tool bubble on its own.
pub fn narrates(row: &Value) -> bool {
    line(row, None).is_some()
}

/// Receipt version of [`ran_line`]: the turn is over, so shell work reads as
/// completed — "Ran", the way gray's own transcript labels it.
fn card_line(tool: &str, detail: &str, elapsed: Option<u64>) -> String {
    shell_line(tool, detail, elapsed, true)
}

fn shell_line(tool: &str, detail: &str, elapsed: Option<u64>, done: bool) -> String {
    let mut out = match tool {
        "bash" | "shell" if done => "💻 Ran".to_string(),
        _ => label(tool),
    };
    // A shell command earns cleaning (no redirections, no `;`-chain essay);
    // a file tool's detail is already a clean path.
    let arg = match tool {
        "bash" | "shell" => clean_command(detail),
        "" => String::new(),
        _ => one_line(detail),
    };
    if !arg.is_empty() {
        out.push(' ');
        out.push_str(&preview(&arg));
    }
    match elapsed {
        Some(ms) => format!("{out} ({})", secs(ms)),
        None => out,
    }
}

fn preview(text: &str) -> String {
    format!("`{}`", capped(text, MAX_PREVIEW_CHARS))
}

fn secs(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// Strip the noise a shell habit leaves in a command line — redirections and
/// everything after the first separator — so a preview reads as one command.
fn clean_command(detail: &str) -> String {
    // `&&` becomes a separator first: splitting on a bare `&` would cut the
    // redirect `2>&1` in half and leave a stray fragment.
    let flat = one_line(detail).replace("&&", ";");
    let parts: Vec<String> = flat
        .split([';', '|'])
        .map(|segment| {
            segment
                .split_whitespace()
                .filter(|token| !is_redirect(token))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|segment| !segment.is_empty())
        .collect();
    let mut out = parts.first().cloned().unwrap_or_default();
    if parts.len() > 1 {
        out.push_str(&format!(" … +{}", parts.len() - 1));
    }
    out
}

/// `2>&1`, `>/dev/null`, a bare `&`: noise a status line must not carry.
fn is_redirect(token: &str) -> bool {
    let digits = token.trim_start_matches(|c: char| c.is_ascii_digit());
    digits.starts_with('>') || digits == "&"
}

fn capped(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Duration keyed by the internal call id, so a line picks up its timing
/// once the call has returned.
fn elapsed_by_call(rows: &[Value]) -> HashMap<String, u64> {
    let mut map = HashMap::new();
    for row in rows {
        if row.get("phase").and_then(Value::as_str) != Some("tool_finished") {
            continue;
        }
        if let (Some(id), Some(ms)) = (
            row.get("call_id").and_then(Value::as_str),
            row.get("elapsed_ms").and_then(Value::as_u64),
        ) {
            map.insert(id.to_string(), ms);
        }
    }
    map
}

fn elapsed_of(row: &Value, by_call: &HashMap<String, u64>) -> Option<u64> {
    row.get("call_id")
        .and_then(Value::as_str)
        .and_then(|id| by_call.get(id).copied())
}

struct BubbleLine {
    text: String,
    /// A `tool_started` placeholder: the matching `tool_ran` overwrites it
    /// instead of adding a second line for the same call.
    start: bool,
    tool: String,
    quiet: bool,
}

/// Render one tool bubble's rows into its body: every distinct line, oldest
/// first (Hermes' accumulating progress bubble). `None` when nothing is
/// worth showing.
pub fn render(rows: &[Value]) -> Option<String> {
    let by_call = elapsed_by_call(rows);
    let mut lines: Vec<BubbleLine> = Vec::new();
    for row in rows {
        let Some(text) = line(row, elapsed_of(row, &by_call)) else {
            continue;
        };
        let tool = row
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let detail = row.get("detail").and_then(Value::as_str).unwrap_or("");
        let quiet = is_quiet_row(&tool, detail);
        let start = row.get("phase").and_then(Value::as_str) == Some("tool_started");
        match lines.last_mut() {
            // One call, one line: its `tool_ran` overwrites the placeholder.
            Some(last) if !start && last.start && last.tool == tool => {
                last.text = text;
                last.start = false;
                last.quiet = quiet;
            }
            // The same call reported twice (a tight loop) says nothing new.
            Some(last) if !start && !last.start && last.text == text => {}
            _ => lines.push(BubbleLine {
                text,
                start,
                tool,
                quiet,
            }),
        }
    }
    let shown: Vec<String> = lines
        .iter()
        .filter(|line| !line.quiet)
        .map(|line| line.text.clone())
        .collect();
    // A pure research turn has nothing but quiet tools; showing those beats
    // showing nothing.
    let shown = if shown.is_empty() {
        lines.into_iter().map(|line| line.text).collect()
    } else {
        shown
    };
    if shown.is_empty() {
        return None;
    }
    Some(shown.join("\n"))
}

#[derive(Default)]
struct CardEntry {
    call_id: String,
    tool: String,
    detail: String,
    elapsed: Option<u64>,
    failed: bool,
}

/// Render the persistent end-of-turn card. Hermes shape: a one-line tally of
/// what the turn did, then the actions themselves — never their output.
pub fn render_card(rows: &[Value]) -> Option<String> {
    let by_call = elapsed_by_call(rows);
    let mut entries: Vec<CardEntry> = Vec::new();
    let mut notices: Vec<String> = Vec::new();
    let mut turn_ms: Option<u64> = None;

    for row in rows {
        let phase = row.get("phase").and_then(Value::as_str).unwrap_or("");
        let tool = row
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let call_id = row
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match phase {
            "tool_ran" => entries.push(CardEntry {
                call_id,
                tool,
                detail: row
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                elapsed: elapsed_of(row, &by_call),
                ..CardEntry::default()
            }),
            "tool_finished" => {
                let failed = row.get("error").and_then(Value::as_bool) == Some(true);
                turn_ms = turn_ms.max(row.get("turn_ms").and_then(Value::as_u64));
                let index = entries.iter().rposition(|entry| {
                    (!call_id.is_empty() && entry.call_id == call_id)
                        || (call_id.is_empty() && entry.tool == tool)
                });
                if let Some(index) = index {
                    entries[index].failed |= failed;
                } else if failed {
                    entries.push(CardEntry {
                        call_id,
                        tool,
                        failed,
                        ..CardEntry::default()
                    });
                }
            }
            "provider_retry" => {
                if let Some(detail) = row.get("detail").and_then(Value::as_str) {
                    if !detail.trim().is_empty() {
                        notices.push(format!("⚠️ {}", one_line(detail)));
                    }
                }
            }
            "compacted" => notices.push("🗜 context compacted".to_string()),
            _ => {}
        }
    }

    if entries.is_empty() && notices.is_empty() {
        return None;
    }

    let mut out = tally(&entries, turn_ms);
    let actions: Vec<&CardEntry> = entries
        .iter()
        .filter(|entry| !is_quiet_row(&entry.tool, &entry.detail))
        .collect();
    for entry in actions.iter().take(MAX_CARD_ROWS) {
        out.push('\n');
        out.push_str(&card_line(&entry.tool, &entry.detail, entry.elapsed));
        if entry.failed {
            out.push_str("\n  ↳ failed");
        }
    }
    let hidden = actions.len().saturating_sub(MAX_CARD_ROWS);
    if hidden > 0 {
        out.push_str(&format!("\n… +{hidden} more"));
    }
    for notice in notices.iter().take(4) {
        out.push('\n');
        out.push_str(notice);
    }

    Some(if out.chars().count() > MAX_CARD_CHARS {
        let mut bounded = out.chars().take(MAX_CARD_CHARS).collect::<String>();
        bounded.push('…');
        bounded
    } else {
        out
    })
}

/// Hermes' post-turn accounting line: `⋯ 12.4s · edited 2 files · read 4
/// files · ran 3 commands`. Counted from the same rows, so it is free.
fn tally(entries: &[CardEntry], turn_ms: Option<u64>) -> String {
    let mut ran = 0usize;
    let mut read = 0usize;
    let mut edited = 0usize;
    let mut other = 0usize;
    for entry in entries {
        match entry.tool.as_str() {
            "bash" | "shell" => ran += 1,
            "read" | "cat" | "find" | "grep" | "glob" | "ls" => read += 1,
            "write" | "create" | "str_replace" | "edit" | "apply_patch" => edited += 1,
            "" => {}
            _ => other += 1,
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if edited > 0 {
        parts.push(format!("edited {edited} file{}", plural(edited)));
    }
    if read > 0 {
        parts.push(format!("read {read} file{}", plural(read)));
    }
    if ran > 0 {
        parts.push(format!("ran {ran} command{}", plural(ran)));
    }
    if other > 0 {
        parts.push(format!("called {other} tool{}", plural(other)));
    }
    if parts.is_empty() {
        return String::new();
    }
    let head = match turn_ms {
        Some(ms) => format!("⋯ {}", secs(ms)),
        None => "⋯".to_string(),
    };
    format!("{head} · {}", parts.join(" · "))
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
