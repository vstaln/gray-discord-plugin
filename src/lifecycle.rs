//! Restart and shutdown notices (Hermes `gateway_restart_notification`
//! parity): chats with a turn in flight hear that it is about to be cut
//! short, and the home channel hears when the gateway is back, including
//! after a crash, kill, or reboot that gave it no chance to say goodbye.
//!
//! State is two files next to the config:
//! - `lifecycle.json`: `running` while the daemon is up, `stopped` after a
//!   clean exit. Still `running` at the next boot means the last run died.
//! - `restart_pending`: written by `gray discord restart` before it signals
//!   the daemon, so a SIGTERM reads as "restarting" instead of "shutting
//!   down" (Hermes' `.restart_pending.json`). The next boot removes it.
use serde_json::{json, Value};
use std::path::Path;

pub const STATE_FILE: &str = "lifecycle.json";
pub const RESTART_MARKER: &str = "restart_pending";

/// How the previous daemon run ended, read once at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Previous {
    /// No record: first boot, nothing to announce.
    FirstBoot,
    /// Exited on a signal after saying so.
    Clean { restart: bool },
    /// Still marked running: crash, SIGKILL, OOM, or power loss.
    Crashed,
}

/// `restart_notification: false` in the config silences every notice.
pub fn enabled(config: &Value) -> bool {
    config
        .get("restart_notification")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// Reads how the last run ended, then records this one as running.
pub fn boot(dir: &Path) -> Previous {
    let restart_marker = dir.join(RESTART_MARKER);
    let restart_requested = restart_marker.exists();
    let _ = std::fs::remove_file(&restart_marker);
    let previous = std::fs::read(dir.join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let previous = match previous {
        None => Previous::FirstBoot,
        Some(state) => match state.get("state").and_then(Value::as_str) {
            Some("stopped") => Previous::Clean {
                restart: restart_requested
                    || state.get("restart").and_then(Value::as_bool) == Some(true),
            },
            _ => Previous::Crashed,
        },
    };
    write(
        dir,
        json!({"state": "running", "at": crate::durable::now_secs()}),
    );
    previous
}

/// Whether a restart (not a plain stop) was asked for.
pub fn restart_requested(dir: &Path) -> bool {
    dir.join(RESTART_MARKER).exists()
}

/// Called by `gray discord restart` before the old process is signalled.
pub fn request_restart(dir: &Path) {
    let _ = std::fs::write(dir.join(RESTART_MARKER), b"");
}

/// Called by `gray discord stop`: a leftover marker from a failed restart
/// must not turn this stop into "restarting".
pub fn clear_restart(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(RESTART_MARKER));
}

/// Records a clean exit; the next boot announces "online", not "crashed".
pub fn mark_stopped(dir: &Path, restart: bool) {
    write(
        dir,
        json!({"state": "stopped", "restart": restart, "at": crate::durable::now_secs()}),
    );
}

fn write(dir: &Path, state: Value) {
    let _ = crate::config::atomic_json(&dir.join(STATE_FILE), &state);
}

/// The home-channel notice for this boot; `interrupted` is how many turns
/// were running when the last run ended.
pub fn startup_notice(previous: Previous, interrupted: usize) -> Option<String> {
    let mut text = match previous {
        Previous::FirstBoot => return None,
        Previous::Clean { restart: true } => "Gateway restarted. gray is back and ready.",
        Previous::Clean { restart: false } => "Gateway online. gray is back and ready.",
        Previous::Crashed => {
            "Gateway is back after an unexpected stop (crash, kill, or reboot). gray is ready."
        }
    }
    .to_string();
    match interrupted {
        0 => {}
        1 => text.push_str("\n1 turn was cut short; send a message in its chat to continue."),
        n => text.push_str(&format!(
            "\n{n} turns were cut short; send a message in their chats to continue."
        )),
    }
    Some(text)
}

/// Sent to each chat whose turn is about to be interrupted.
pub fn active_notice(restart: bool) -> &'static str {
    if restart {
        "Gateway restarting: your current task will be interrupted. Send any message after the restart to pick up where you left off."
    } else {
        "Gateway shutting down: your current task will be interrupted. Once it is back online, send any message to pick up where you left off."
    }
}

/// Sent to the home channel when nothing is running there.
pub fn home_notice(restart: bool) -> &'static str {
    if restart {
        "Gateway restarting. Back in a moment."
    } else {
        "Gateway shutting down."
    }
}

/// Who hears about a shutdown: every chat with a running turn, then the
/// home channel unless it was already told. One message per chat.
pub fn shutdown_targets(running: &[String], home: &str, restart: bool) -> Vec<(u64, &'static str)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for channel in running {
        if let Ok(ch) = channel.parse::<u64>() {
            if seen.insert(ch) {
                out.push((ch, active_notice(restart)));
            }
        }
    }
    if let Ok(ch) = home.parse::<u64>() {
        if seen.insert(ch) {
            out.push((ch, home_notice(restart)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_boot_is_silent_then_a_crash_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(boot(tmp.path()), Previous::FirstBoot);
        // Never marked stopped: the run died.
        assert_eq!(boot(tmp.path()), Previous::Crashed);
        assert!(startup_notice(Previous::Crashed, 2)
            .unwrap()
            .contains("2 turns were cut short"));
        assert_eq!(startup_notice(Previous::FirstBoot, 3), None);
    }

    #[test]
    fn a_requested_restart_reads_as_restart_once() {
        let tmp = tempfile::tempdir().unwrap();
        boot(tmp.path());
        request_restart(tmp.path());
        assert!(restart_requested(tmp.path()));
        mark_stopped(tmp.path(), true);
        assert_eq!(boot(tmp.path()), Previous::Clean { restart: true });
        assert!(!restart_requested(tmp.path()));
        mark_stopped(tmp.path(), false);
        assert_eq!(boot(tmp.path()), Previous::Clean { restart: false });
    }

    #[test]
    fn each_chat_hears_once_and_home_gets_the_short_notice() {
        let running = vec!["11".to_string(), "11".to_string(), "22".to_string()];
        let targets = shutdown_targets(&running, "22", true);
        assert_eq!(
            targets,
            vec![(11, active_notice(true)), (22, active_notice(true))]
        );
        let targets = shutdown_targets(&[], "33", false);
        assert_eq!(targets, vec![(33, home_notice(false))]);
    }

    #[test]
    fn notices_can_be_turned_off() {
        assert!(enabled(&json!({})));
        assert!(!enabled(&json!({"restart_notification": false})));
    }
}
