//! Turn narration for chat surfaces.
//!
//! gray's `--json` progress rows are platform-agnostic: a tool name, a
//! redacted one-line detail, and (for completed tools) a bounded redacted
//! output. This module is the shared plumbing between the runner (which reads
//! those rows) and the gateway (which knows the channel): a bounded, scoped
//! sink plus pure renderers for the live status bubble and the persistent
//! tool-activity card.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// Bounded so a runaway turn cannot grow the queue without limit. 64 pending
/// rows is far more than one status bubble shows; the oldest fall off.
const MAX_PENDING: usize = 64;
/// Keep a bounded history for the final card even after the live rows have
/// been drained. This is intentionally larger than the pending queue so a
/// turn with many quick tools still produces a useful transcript.
const MAX_HISTORY: usize = 128;
/// Do not let one turn turn into an unbounded wall of Discord messages.
const MAX_CARD_ROWS: usize = 16;
const MAX_CARD_OUTPUT: usize = 1600;
const MAX_CARD_CHARS: usize = 10_000;

/// Lines kept in the bubble: the current one plus two before it, so a
/// pause reads as a sequence rather than a single blinking line.
const KEEP_LINES: usize = 3;

/// Seconds between edits of the same bubble (Discord rate-limits edits).
const MIN_EDIT_GAP: f64 = 1.0;

#[derive(Default)]
struct ScopeState {
    pending: VecDeque<Value>,
    history: VecDeque<Value>,
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

/// Start a fresh history for one conversation. The daemon may have a stale
/// entry after a crash or a cancelled turn; never let it bleed into the next
/// turn's card.
pub fn begin(s: &Sink, scope: &str) {
    if let Ok(mut state) = s.lock() {
        state
            .scopes
            .insert(scope.to_string(), ScopeState::default());
    }
}

pub fn push(s: &Sink, row: Value) {
    push_for(s, "", row);
}

pub fn push_for(s: &Sink, scope: &str, row: Value) {
    if let Ok(mut state) = s.lock() {
        let scope = state.scopes.entry(scope.to_string()).or_default();
        if scope.pending.len() >= MAX_PENDING {
            scope.pending.pop_front();
        }
        scope.pending.push_back(row.clone());
        if scope.history.len() >= MAX_HISTORY {
            scope.history.pop_front();
        }
        scope.history.push_back(row);
    }
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

/// One line for one row, or `None` when the row adds nothing a reader
/// needs. A finished tool with no error is silence — the `ran` line above
/// it already said what happened.
fn line(row: &Value) -> Option<String> {
    let phase = row.get("phase").and_then(Value::as_str).unwrap_or("");
    let tool = row.get("tool").and_then(Value::as_str).unwrap_or("");
    let detail = row.get("detail").and_then(Value::as_str).unwrap_or("");
    match phase {
        // Hermes parity: `💻 terminal: ls`, `📖 Reading config.yaml L1-30`.
        // Start has no arguments yet, but it is still important: a long
        // shell/tool call should show that work began before it returns.
        "tool_started" => Some(started_line(tool)),
        "tool_ran" => Some(ran_line(tool, detail)),
        "tool_finished" if row.get("error").and_then(Value::as_bool) == Some(true) => Some(
            format!("❌ {} failed", if tool.is_empty() { "tool" } else { tool }),
        ),
        "provider_retry" if !detail.is_empty() => Some(format!("⚠️ {}", one_line(detail))),
        "compacted" => Some("🗜 context compacted".to_string()),
        _ => None,
    }
}

fn started_line(tool: &str) -> String {
    match tool {
        "bash" | "shell" => "💻 terminal".to_string(),
        "read" | "cat" => "📖 Reading a file".to_string(),
        "write" | "create" | "str_replace" => "✍️ Writing a file".to_string(),
        "edit" | "apply_patch" => "✍️ Editing a file".to_string(),
        "" => "🔧 working".to_string(),
        other => format!("🔧 {other}"),
    }
}

fn ran_line(tool: &str, detail: &str) -> String {
    match tool {
        // The detail of a file tool is already a clean path (plus a
        // line range for reads), so it is the whole headline.
        "read" | "cat" => format!("📖 Reading {}", named(detail, "a file")),
        "write" | "create" | "str_replace" => {
            format!("✍️ Writing {}", named(detail, "a file"))
        }
        "edit" | "apply_patch" => format!("✍️ Editing {}", named(detail, "a file")),
        "bash" | "shell" => {
            if detail.is_empty() {
                "💻 terminal".to_string()
            } else {
                format!("💻 terminal: {}", one_line(detail))
            }
        }
        "" => "🔧 working".to_string(),
        other if detail.is_empty() => format!("🔧 {other}"),
        other => format!("🔧 {other}: {}", one_line(detail)),
    }
}

fn named(detail: &str, fallback: &str) -> String {
    if detail.is_empty() {
        fallback.to_string()
    } else {
        one_line(detail)
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render a drained batch into the bubble body: the last `KEEP_LINES`
/// distinct lines, oldest first. `None` when nothing is worth showing.
pub fn render(rows: &[Value]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for row in rows {
        if let Some(l) = line(row) {
            // Collapse repeats (a tight tool loop re-reports the same phase).
            if lines.last().map(String::as_str) != Some(l.as_str()) {
                lines.push(l);
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let skip = lines.len().saturating_sub(KEEP_LINES);
    Some(lines[skip..].join("\n"))
}

#[derive(Default)]
struct CardEntry {
    call_id: String,
    tool: String,
    detail: String,
    output: Option<String>,
    failed: bool,
}

/// Render the persistent end-of-turn transcript. This intentionally includes
/// only tool lifecycle rows and bounded output: reasoning is never part of a
/// chat activity surface.
pub fn render_card(rows: &[Value]) -> Option<String> {
    let mut entries: Vec<CardEntry> = Vec::new();
    let mut notices: Vec<String> = Vec::new();

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
                ..CardEntry::default()
            }),
            "tool_finished" => {
                let output = row
                    .get("output")
                    .and_then(Value::as_str)
                    .map(bounded_output)
                    .filter(|s| !s.is_empty());
                let failed = row.get("error").and_then(Value::as_bool) == Some(true);
                let index = entries.iter().rposition(|entry| {
                    (!call_id.is_empty() && entry.call_id == call_id)
                        || (call_id.is_empty() && entry.tool == tool)
                });
                if let Some(index) = index {
                    let entry = &mut entries[index];
                    if entry.tool.is_empty() {
                        entry.tool = tool;
                    }
                    if output.is_some() {
                        entry.output = output;
                    }
                    entry.failed |= failed;
                } else if output.is_some() || failed {
                    entries.push(CardEntry {
                        call_id,
                        tool,
                        output,
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

    let mut out = String::from("🛠 Tool activity");
    for entry in entries.iter().take(MAX_CARD_ROWS) {
        out.push('\n');
        if entry.tool == "bash" || entry.tool == "shell" {
            if entry.detail.is_empty() {
                out.push('$');
            } else {
                out.push_str("$ ");
                out.push_str(&one_line(&entry.detail));
            }
        } else {
            let tool = if entry.tool.is_empty() {
                "tool"
            } else {
                &entry.tool
            };
            out.push('•');
            out.push(' ');
            out.push_str(tool);
            if !entry.detail.is_empty() {
                out.push_str(": ");
                out.push_str(&one_line(&entry.detail));
            }
        }
        if entry.failed {
            out.push_str("\n  ↳ failed");
        }
        if let Some(output) = &entry.output {
            out.push('\n');
            out.push_str(&fenced(output));
        }
    }
    if entries.len() > MAX_CARD_ROWS {
        out.push_str("\n… older tool activity omitted");
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

fn bounded_output(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.chars().count() > MAX_CARD_OUTPUT {
        format!(
            "{}…",
            normalized.chars().take(MAX_CARD_OUTPUT).collect::<String>()
        )
    } else {
        normalized
    }
}

fn fenced(output: &str) -> String {
    let longest = output.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}text\n{}\n{fence}", output.trim_end())
}

/// True when `text` is worth an edit: it changed, and the gap since the
/// last edit has passed. Content equality is the real gate — Discord
/// rejects a no-op edit, and a quiet turn should send nothing at all.
pub fn should_publish(text: &str, last: Option<&str>, now: f64, last_at: f64) -> bool {
    if Some(text) == last {
        return false;
    }
    last_at < 0.0 || now - last_at >= MIN_EDIT_GAP
}
