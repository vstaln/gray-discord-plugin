//! Port of gray_discord/config.py — see implementation plan Task list.
use std::path::PathBuf;

/// Default config path: ~/.config/gray-discord/config.json (Task 1 stub;
/// full module with validation lands in Task 2).
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/gray-discord/config.json")
}
