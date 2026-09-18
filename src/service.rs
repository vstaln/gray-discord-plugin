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

/// systemctl passthrough.
pub fn control<S: AsRef<str>>(args: &[S]) -> Result<(), String> {
    let mut cmd = std::process::Command::new("systemctl");
    cmd.arg("--user");
    for a in args {
        cmd.arg(a.as_ref());
    }
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        _ => Err("systemctl failed; check the user session and service status".to_string()),
    }
}

/// Install + enable the user unit.
pub fn install(path: &Path) -> Result<(), String> {
    let target = unit_path();
    let body = unit(path)?;
    if target.exists() {
        let existing =
            std::fs::read_to_string(&target).map_err(|_| "cannot read unit".to_string())?;
        if existing != body {
            return Err(
                "A different plugin service already exists; uninstall it first".to_string(),
            );
        }
    }
    if let Some(p) = target.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .create(p)
            .map_err(|_| "cannot create systemd unit directory".to_string())?;
    }
    std::fs::write(&target, body).map_err(|_| "cannot write unit file".to_string())?;
    control(&["daemon-reload"])?;
    control(&["enable", "--now", NAME])?;
    Ok(())
}

/// Disable + remove the user unit.
pub fn uninstall() -> Result<(), String> {
    let target = unit_path();
    if !target.exists() {
        return Ok(());
    }
    control(&["disable", "--now", NAME])?;
    let _ = std::fs::remove_file(&target);
    control(&["daemon-reload"])?;
    Ok(())
}
