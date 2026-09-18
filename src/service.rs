//! Port of gray_discord/service.py: systemd user lifecycle + unit quoting.
//! Never places tokens in unit files. Task 6 ships `NAME`/`quote`/`unit`
//! plus `control`/`install`/`uninstall` stubs; Task 11 wires systemctl.
use std::path::{Path, PathBuf};

/// Systemd user unit name (unchanged from the Python plugin).
pub const NAME: &str = "gray-discord-plugin.service";

/// User systemd directory (`$XDG_CONFIG_HOME` or `~/.config`).
pub fn unit_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("systemd/user").join(NAME)
}

/// Quote one argv word for systemd `ExecStart` (Python parity: backslash,
/// quote, `%`, `$` escaped; newline/CR/NUL rejected).
pub fn quote(value: &str) -> Result<String, String> {
    if value.contains(['\n', '\r', '\0']) {
        return Err("Invalid service path".to_string());
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

/// Render the user unit. Argv uses the current Rust binary — replaces the
/// Python's `sys.executable -m gray_discord`.
pub fn unit(config_path: &Path) -> Result<String, String> {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| "cannot locate service binary".to_string())?;
    let resolved = config_path
        .to_str()
        .map(str::to_string)
        .unwrap_or_else(|| config_path.to_string_lossy().into_owned());
    // Field-for-field parity with service.py (After, Type, Restart, UMask,
    // KillMode, TimeoutStopSec, WantedBy); only the argv changes: the Rust
    // binary takes `<exe> run --config <path>` (subcommand-first clap CLI)
    // instead of `<python> -m gray_discord --config <path> run`.
    let words = [exe.as_str(), "run", "--config", resolved.as_str()];
    let mut exec: Vec<String> = Vec::with_capacity(words.len());
    for w in words {
        exec.push(quote(w)?);
    }
    Ok(format!(
        "[Unit]\nDescription=gray Discord plugin\nAfter=network-online.target\n\
         [Service]\nType=simple\nExecStart={}\nRestart=on-failure\nRestartSec=10\n\
         UMask=0077\nKillMode=control-group\nTimeoutStopSec=15\n\
         [Install]\nWantedBy=default.target\n",
        exec.join(" ")
    ))
}

/// systemctl passthrough (Task 11).
pub fn control(_args: &[String]) -> Result<(), String> {
    Err("not yet implemented".to_string())
}

/// Install + enable the user unit (Task 11).
pub fn install(_path: &Path) -> Result<(), String> {
    Err("not yet implemented".to_string())
}

/// Disable + remove the user unit (Task 11).
pub fn uninstall() -> Result<(), String> {
    Err("not yet implemented".to_string())
}
