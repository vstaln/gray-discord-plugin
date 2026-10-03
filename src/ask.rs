//! Questions the agent asks the person on Discord (`discord_ask`).
//!
//! gray's own `host/ask` has no human on the other end in `-p` mode, so the
//! Discord sidecar offers a tool instead: it posts a question card in the
//! turn's channel and waits for the answer. The card is Components V2 and
//! plain text:
//!
//! ```text
//! ┃ -# Question from gray
//! ┃ **Branch** · Which branch should I deploy?
//! ┃ [main] [staging] [Other…]                ← buttons (up to 4 options)
//! ┃ ───────────────────────────────────────
//! ┃ -# Waiting for your answer · expires in 5 minutes · or reply in this channel
//! ```
//!
//! Up to 4 options are buttons; more, or a multiple choice, is a select
//! menu. "Other…" (or "Answer…" when there are no options) opens a modal
//! with a text box. A plain message in the channel answers every open
//! question too. The gateway records presses in the store and redraws the
//! card in the same interaction response; the sidecar polls the store and
//! hands the answers back to the agent.
//!
//! Button `custom_id`s are `ask:<ask id>:<question>:<choice>`. The ask id is
//! 64 random bits; a press is honored only on an open card, in its own
//! channel, from an admitted user (the gateway admits before routing).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How long a card waits for an answer. Under gray's 300-second ask budget
/// for a protocol-1.1 sidecar tool, so the tool reports its own timeout.
pub const ASK_SECS: u64 = 280;
const MAX_QUESTIONS: usize = 3;
const MAX_OPTIONS: usize = 25;
/// Up to this many options render as buttons (plus "Other…" fills a row).
const BUTTON_OPTIONS: usize = 4;

const OPEN: u32 = 0xFEE75C;
const ANSWERED: u32 = 0x57F287;
const CLOSED: u32 = 0x80848E;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<AskOption>,
    /// Allow picking several options.
    #[serde(default)]
    pub multiple: bool,
}

/// Validate the tool's `questions` argument (gray-questions' shape).
pub fn parse(args: &Value) -> Result<Vec<Question>, String> {
    let questions: Vec<Question> = serde_json::from_value(
        args.get("questions")
            .cloned()
            .ok_or("questions is required")?,
    )
    .map_err(|e| format!("questions is malformed: {e}"))?;
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(format!("ask 1-{MAX_QUESTIONS} questions"));
    }
    let mut seen = Vec::new();
    for q in &questions {
        if q.id.trim().is_empty() || q.id.len() > 40 || q.id.contains(':') {
            return Err("each question needs an id of 1-40 characters, no ':'".into());
        }
        if seen.contains(&&q.id) {
            return Err(format!("question id {} is used twice", q.id));
        }
        seen.push(&q.id);
        if q.question.trim().is_empty() || q.question.chars().count() > 1000 {
            return Err("each question needs text of 1-1000 characters".into());
        }
        if q.header.chars().count() > 45 {
            return Err("a header is at most 45 characters".into());
        }
        if q.options.len() > MAX_OPTIONS {
            return Err(format!("at most {MAX_OPTIONS} options per question"));
        }
        for option in &q.options {
            if option.label.trim().is_empty() || option.label.chars().count() > 80 {
                return Err("each option label is 1-80 characters".into());
            }
            if option.description.chars().count() > 100 {
                return Err("an option description is at most 100 characters".into());
            }
        }
    }
    Ok(questions)
}

/// A press on a question card, from its `custom_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Press {
    pub ask_id: String,
    pub question: usize,
    pub choice: Choice,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Option(usize),
    Select,
    /// Open the text box.
    Other,
    /// The text box was submitted.
    Note,
}

pub fn parse_press(custom_id: &str) -> Option<Press> {
    let mut parts = custom_id.strip_prefix("ask:")?.split(':');
    let ask_id = parts.next()?.to_string();
    let question = parts.next()?.parse().ok()?;
    let choice = match parts.next()? {
        "select" => Choice::Select,
        "other" => Choice::Other,
        "note" => Choice::Note,
        index => Choice::Option(index.parse().ok()?),
    };
    if parts.next().is_some() || ask_id.is_empty() {
        return None;
    }
    Some(Press {
        ask_id,
        question,
        choice,
    })
}

/// Free text is marked the way gray-questions marks it.
pub fn note(text: &str) -> String {
    format!("user_note: {}", text.trim())
}

/// The answers for one press, or `None` for an unknown option.
pub fn answers_for(question: &Question, choice: &Choice, values: &[String]) -> Option<Vec<String>> {
    match choice {
        Choice::Option(index) => question
            .options
            .get(*index)
            .map(|option| vec![option.label.clone()]),
        Choice::Select => {
            let picked: Vec<String> = values
                .iter()
                .filter_map(|value| value.parse::<usize>().ok())
                .filter_map(|index| question.options.get(index))
                .map(|option| option.label.clone())
                .collect();
            (!picked.is_empty()).then_some(picked)
        }
        Choice::Other | Choice::Note => None,
    }
}

/// The question card for `row` as it stands.
pub fn render(row: &crate::durable::AskRow) -> Vec<Value> {
    let questions: Vec<Question> =
        serde_json::from_value(row.questions.clone()).unwrap_or_default();
    let open = row.state == "open";
    let accent = match row.state.as_str() {
        "open" => OPEN,
        "answered" => ANSWERED,
        _ => CLOSED,
    };
    let heading = if questions.len() == 1 {
        "-# Question from gray"
    } else {
        "-# Questions from gray"
    };
    let mut children = vec![json!({"type": 10, "content": heading})];
    for (index, q) in questions.iter().enumerate() {
        if index > 0 {
            children.push(json!({"type": 14, "divider": false, "spacing": 1}));
        }
        let title = if q.header.trim().is_empty() {
            format!("**{}**", q.question.trim())
        } else {
            format!("**{}** · {}", q.header.trim(), q.question.trim())
        };
        children.push(json!({"type": 10, "content": title}));
        let answered = row.answers.get(&q.id).and_then(Value::as_array);
        if let Some(answers) = answered {
            let shown: Vec<String> = answers
                .iter()
                .filter_map(Value::as_str)
                .map(|answer| {
                    answer
                        .strip_prefix("user_note: ")
                        .unwrap_or(answer)
                        .to_string()
                })
                .collect();
            children
                .push(json!({"type": 10, "content": format!("-# Answer: {}", shown.join(", "))}));
            continue;
        }
        if !open {
            continue;
        }
        let buttons = q.options.len() <= BUTTON_OPTIONS && !q.multiple;
        let described: Vec<String> = q
            .options
            .iter()
            .filter(|option| buttons && !option.description.trim().is_empty())
            .map(|option| {
                format!(
                    "-# **{}**: {}",
                    option.label.trim(),
                    option.description.trim()
                )
            })
            .collect();
        if !described.is_empty() {
            children.push(json!({"type": 10, "content": described.join("\n")}));
        }
        let id = |choice: &str| format!("ask:{}:{index}:{choice}", row.ask_id);
        let other_label = if q.options.is_empty() {
            "Answer…"
        } else {
            "Other…"
        };
        let other = json!({"type": 2, "style": 2, "label": other_label, "custom_id": id("other")});
        if buttons {
            let mut row_buttons: Vec<Value> = q
                .options
                .iter()
                .enumerate()
                .map(|(o, option)| {
                    json!({"type": 2, "style": 2, "label": option.label.trim(), "custom_id": id(&o.to_string())})
                })
                .collect();
            row_buttons.push(other);
            children.push(json!({"type": 1, "components": row_buttons}));
        } else {
            let options: Vec<Value> = q
                .options
                .iter()
                .enumerate()
                .map(|(o, option)| {
                    let mut item = json!({"label": option.label.trim(), "value": o.to_string()});
                    if !option.description.trim().is_empty() {
                        item["description"] = json!(option.description.trim());
                    }
                    item
                })
                .collect();
            let max = if q.multiple { q.options.len() } else { 1 };
            children.push(json!({"type": 1, "components": [{
                "type": 3,
                "custom_id": id("select"),
                "placeholder": if q.multiple { "Choose any…" } else { "Choose one…" },
                "options": options,
                "min_values": 1,
                "max_values": max,
            }]}));
            children.push(json!({"type": 1, "components": [other]}));
        }
    }
    let footer = match row.state.as_str() {
        "open" => format!(
            "-# Waiting for your answer · expires <t:{}:R> · or reply in this channel",
            row.expires as i64
        ),
        "answered" => "-# Answered".to_string(),
        "expired" => "-# No answer in time; gray went ahead on its own judgement".to_string(),
        _ => "-# The turn ended before an answer".to_string(),
    };
    children.push(json!({"type": 14, "divider": true, "spacing": 1}));
    children.push(json!({"type": 10, "content": footer}));
    vec![json!({"type": 17, "accent_color": accent, "components": children})]
}

/// The modal for "Other…" / "Answer…": one paragraph text box.
pub fn note_modal(row: &crate::durable::AskRow, question: usize) -> Option<Value> {
    let questions: Vec<Question> = serde_json::from_value(row.questions.clone()).ok()?;
    let q = questions.get(question)?;
    let label: String = if q.header.trim().is_empty() {
        "Your answer".to_string()
    } else {
        q.header.trim().chars().take(45).collect()
    };
    let description: String = q.question.trim().chars().take(100).collect();
    Some(json!({
        "custom_id": format!("ask:{}:{question}:note", row.ask_id),
        "title": "Answer gray",
        "components": [{
            "type": 18,
            "label": label,
            "description": description,
            "component": {
                "type": 4,
                "custom_id": "note",
                "style": 2,
                "min_length": 1,
                "max_length": 1000,
            },
        }],
    }))
}

/// The tool result for the agent: who answered what, in one JSON object
/// keyed by question id (gray-questions' `answers` shape).
pub fn result_text(row: &crate::durable::AskRow) -> String {
    let answers: serde_json::Map<String, Value> = row
        .answers
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(id, answers)| (id.clone(), json!({"answers": answers})))
                .collect()
        })
        .unwrap_or_default();
    let json = json!({"answers": answers});
    match row.state.as_str() {
        "answered" => format!("The person answered on Discord.\n{json}"),
        "expired" => format!(
            "No answer within {} minutes. Use your best judgement and say what you assumed.\n{json}",
            ASK_SECS / 60
        ),
        _ => format!("The question was closed before an answer.\n{json}"),
    }
}

/// Post the card in `channel` and wait for the answer: until every
/// question has one, `wait` runs out (the card closes as expired), or the
/// gray process that called us is gone (closed as cancelled). Returns the
/// tool result for the agent.
pub async fn ask(
    rest: &crate::transport::Rest,
    store: &crate::durable::Store,
    channel: u64,
    questions: &[Question],
    wait: std::time::Duration,
    poll: std::time::Duration,
) -> Value {
    let ask_id = crate::durable::uuid_hex()[..16].to_string();
    let expires = crate::durable::now_secs() + wait.as_secs_f64();
    let encoded = json!(questions);
    let row = match store
        .ask_create(&ask_id, &channel.to_string(), &encoded, expires)
        .and_then(|()| store.ask_get(&ask_id))
    {
        Ok(Some(row)) => row,
        _ => return failed("the question could not be stored"),
    };
    let message = match rest.send_v2(channel, &render(&row), None).await {
        Ok(id) => id,
        Err(error) => {
            let _ = store.ask_close(&ask_id, "cancelled");
            return failed(&format!("Discord refused the question card: {error}"));
        }
    };
    let _ = store.ask_set_message(&ask_id, &message);
    let parent = parent_pid();
    let deadline = tokio::time::Instant::now() + wait;
    let row = loop {
        tokio::time::sleep(poll).await;
        let current = store.ask_get(&ask_id).ok().flatten();
        if let Some(row) = current.as_ref().filter(|row| row.state != "open") {
            break row.clone();
        }
        let closing = if tokio::time::Instant::now() >= deadline {
            Some("expired")
        } else if parent_pid() != parent {
            Some("cancelled")
        } else {
            None
        };
        if let Some(state) = closing {
            match store.ask_close(&ask_id, state) {
                Ok(Some(row)) => {
                    let _ = rest.edit_v2(channel, &message, &render(&row)).await;
                    break row;
                }
                // Answered in the same instant: take the answer.
                _ => match store.ask_get(&ask_id).ok().flatten() {
                    Some(row) => break row,
                    None => return failed("the question disappeared"),
                },
            }
        }
    };
    json!({"content": result_text(&row)})
}

fn failed(detail: &str) -> Value {
    json!({
        "content": format!("Not asked: {detail}. Nothing is waiting on Discord; continue on your own judgement."),
        "is_error": true
    })
}

/// A plain message in a channel with an open card answers every question
/// still open on it, in the person's own words. True when the message was
/// taken as an answer (and must not start a turn of its own).
pub async fn answer_typed(
    store: &crate::durable::Store,
    rest: &crate::transport::Rest,
    channel: &str,
    text: &str,
) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let now = crate::durable::now_secs();
    let Ok(Some(row)) = store.ask_open_in(channel, now) else {
        return false;
    };
    let questions: Vec<Question> =
        serde_json::from_value(row.questions.clone()).unwrap_or_default();
    let mut updated = None;
    for q in questions
        .iter()
        .filter(|q| row.answers.get(&q.id).is_none())
    {
        if let Ok(Some(next)) = store.ask_answer(&row.ask_id, &q.id, &[note(text)], now) {
            updated = Some(next);
        }
    }
    let Some(updated) = updated else {
        return false;
    };
    if let (Some(message), Ok(ch)) = (updated.message_id.as_deref(), channel.parse::<u64>()) {
        let _ = rest.edit_v2(ch, message, &render(&updated)).await;
    }
    true
}

/// The sidecar's parent. When the gray process that spawned it exits (the
/// turn was stopped or timed out) the sidecar is reparented, to init or a
/// subreaper such as systemd's user manager: nobody waits for the answer.
fn parent_pid() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: getppid has no preconditions.
        i64::from(unsafe { libc::getppid() })
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable::AskRow;

    fn row(questions: Value, answers: Value, state: &str) -> AskRow {
        AskRow {
            ask_id: "abc123".into(),
            channel: "42".into(),
            message_id: None,
            questions,
            answers,
            state: state.into(),
            expires: 1_700_000_300.0,
        }
    }

    fn branch() -> Value {
        json!([{
            "id": "branch", "header": "Branch", "question": "Which branch should I deploy?",
            "options": [{"label": "main", "description": "production"}, {"label": "staging"}]
        }])
    }

    #[test]
    fn a_few_options_are_buttons_plus_other() {
        let card = render(&row(branch(), json!({}), "open"));
        crate::render::validate_components(&card).unwrap();
        let text = card[0].to_string();
        assert!(text.contains("**Branch** · Which branch should I deploy?"));
        assert!(text.contains("-# **main**: production"));
        assert!(text.contains("\"custom_id\":\"ask:abc123:0:0\""));
        assert!(text.contains("\"custom_id\":\"ask:abc123:0:1\""));
        assert!(text.contains("\"label\":\"Other…\""));
        assert!(text.contains("expires <t:1700000300:R>"));
        assert_eq!(card[0]["accent_color"], json!(OPEN));
        assert!(!text.contains("emoji"), "plain text, no emojis");
    }

    #[test]
    fn many_options_or_a_multiple_choice_use_a_select() {
        let options: Vec<Value> = (0..6).map(|i| json!({"label": format!("o{i}")})).collect();
        let qs = json!([{"id": "pick", "question": "Pick", "options": options, "multiple": true}]);
        let card = render(&row(qs, json!({}), "open"));
        crate::render::validate_components(&card).unwrap();
        let select = card[0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == 1 && c["components"][0]["type"] == 3)
            .unwrap();
        assert_eq!(select["components"][0]["max_values"], 6);
        assert_eq!(select["components"][0]["custom_id"], "ask:abc123:0:select");
    }

    #[test]
    fn an_answered_card_shows_answers_and_no_controls() {
        let card = render(&row(
            branch(),
            json!({"branch": ["user_note: the hotfix branch"]}),
            "answered",
        ));
        crate::render::validate_components(&card).unwrap();
        let text = card[0].to_string();
        assert!(text.contains("-# Answer: the hotfix branch"));
        assert!(!text.contains("ask:abc123"), "no buttons left");
        assert_eq!(card[0]["accent_color"], json!(ANSWERED));
    }

    #[test]
    fn presses_parse_and_map_to_answers() {
        let press = parse_press("ask:abc123:0:1").unwrap();
        assert_eq!(press.choice, Choice::Option(1));
        let q: Vec<Question> = serde_json::from_value(branch()).unwrap();
        assert_eq!(
            answers_for(&q[0], &press.choice, &[]),
            Some(vec!["staging".into()])
        );
        assert_eq!(
            answers_for(&q[0], &Choice::Select, &["0".into(), "9".into()]),
            Some(vec!["main".into()])
        );
        assert_eq!(
            parse_press("ask:abc123:0:other").unwrap().choice,
            Choice::Other
        );
        assert!(parse_press("ask::0:1").is_none());
        assert!(parse_press("turn:stop:x").is_none());
    }

    #[test]
    fn the_text_box_modal_is_a_label_with_a_paragraph() {
        let modal = note_modal(&row(branch(), json!({}), "open"), 0).unwrap();
        assert_eq!(modal["custom_id"], "ask:abc123:0:note");
        assert_eq!(modal["components"][0]["type"], 18);
        assert_eq!(modal["components"][0]["component"]["style"], 2);
    }

    #[test]
    fn bad_questions_are_refused_before_anything_is_sent() {
        assert!(parse(&json!({"questions": []})).is_err());
        assert!(parse(&json!({"questions": [{"id": "a:b", "question": "x"}]})).is_err());
        let dup =
            json!({"questions": [{"id": "a", "question": "x"}, {"id": "a", "question": "y"}]});
        assert!(parse(&dup).is_err());
        assert!(parse(&json!({"questions": [{"id": "a", "question": "x"}]})).is_ok());
    }

    #[test]
    fn the_result_carries_answers_by_question_id() {
        let text = result_text(&row(branch(), json!({"branch": ["main"]}), "answered"));
        assert!(text.contains("{\"answers\":{\"branch\":{\"answers\":[\"main\"]}}}"));
    }
}
