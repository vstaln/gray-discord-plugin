//! Turn narration for chat surfaces.
//!
//! gray's `--json` progress rows are platform-agnostic: a tool name, a
//! redacted one-line detail, reasoning batches. This module is the shared
//! plumbing between the runner (which reads those rows) and the gateway
//! (which knows the channel): a bounded sink plus a pure renderer that
//! turns a batch of rows into the single status bubble Hermes shows —
//! one message, overwritten as the agent works, not one message per tool
//! call.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// Bounded so a runaway turn cannot grow the queue without limit. 64 rows
/// is far more than one status bubble shows; the oldest fall off.
const MAX_PENDING: usize = 64;

/// Lines kept in the bubble: the current one plus two before it, so a
/// pause reads as a sequence rather than a single blinking line.
const KEEP_LINES: usize = 3;

/// Seconds between edits of the same bubble (Discord rate-limits edits).
const MIN_EDIT_GAP: f64 = 1.0;

pub type Sink = Arc<Mutex<VecDeque<Value>>>;

pub fn sink() -> Sink {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Narration on/off. Default on, same semantics as `typing_indicator`:
/// absent means enabled, only an explicit `false` turns it off.
pub fn enabled(config: &Value) -> bool {
    config
        .get("activity_indicator")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

pub fn push(s: &Sink, row: Value) {
    if let Ok(mut q) = s.lock() {
        if q.len() >= MAX_PENDING {
            q.pop_front();
        }
        q.push_back(row);
    }
}

pub fn drain(s: &Sink) -> Vec<Value> {
    let mut q = match s.lock() {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };
    q.drain(..).collect()
}

/// The runner's progress callback: push, never block, never fail a turn.
pub fn callback<'a>(s: Sink) -> crate::runner::ProgressFn<'a> {
    Box::new(move |row: &Value| push(&s, row.clone()))
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
        "tool_ran" => Some(match tool {
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
        }),
        "tool_finished" if row.get("error").and_then(Value::as_bool) == Some(true) => Some(
            format!("❌ {} failed", if tool.is_empty() { "tool" } else { tool }),
        ),
        "provider_retry" if !detail.is_empty() => Some(format!("⚠️ {}", one_line(detail))),
        "compacted" => Some("🗜 context compacted".to_string()),
        _ => None,
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

/// True when `text` is worth an edit: it changed, and the gap since the
/// last edit has passed. Content equality is the real gate — Discord
/// rejects a no-op edit, and a quiet turn should send nothing at all.
pub fn should_publish(text: &str, last: Option<&str>, now: f64, last_at: f64) -> bool {
    if Some(text) == last {
        return false;
    }
    last_at < 0.0 || now - last_at >= MIN_EDIT_GAP
}
