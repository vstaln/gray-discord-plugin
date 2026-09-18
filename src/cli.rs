//! CLI tree: `gray-discord --config <path> <subcommand>`. Mirrors gray_discord/cli.py parser().
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Standalone Discord transport and setup for gray.
#[derive(Debug, Parser)]
#[command(name = "gray-discord", bin_name = "gray discord")]
pub struct Cli {
    /// Config file path.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn config_path(&self) -> PathBuf {
        match &self.config {
            Some(p) => p.clone(),
            None => crate::config::default_path(),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    Setup,
    Run,
    Sidecar,
    Register,
    Install,
    Status,
    Stop,
    Restart,
    Doctor,
    Uninstall,
    Limits {
        #[arg(long)]
        timeout_seconds: Option<u64>,
        #[arg(long)]
        concurrency: Option<u64>,
        #[arg(long)]
        max_requests: Option<u64>,
    },
    Budget {
        #[command(subcommand)]
        action: BudgetAction,
    },
    Share {
        #[arg(long)]
        skill: Vec<String>,
        #[arg(long)]
        context: Vec<String>,
        #[arg(long, name = "plugin-argv")]
        plugin_argv: Vec<String>,
        #[arg(long)]
        clear: bool,
    },
    Queue {
        #[command(subcommand)]
        action: QueueAction,
    },
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    Allowlist {
        #[command(subcommand)]
        action: AllowlistAction,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum BudgetAction {
    Status,
    Set {
        #[arg(long, name = "daily-usd")]
        daily_usd: f64,
        #[arg(long, name = "turn-usd")]
        turn_usd: f64,
        #[arg(long, name = "input-per-million")]
        input_per_million: f64,
        #[arg(long, name = "output-per-million")]
        output_per_million: f64,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum QueueAction {
    List,
    Cancel { id: String },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ScheduleAction {
    Add {
        #[arg(long)]
        every: u64,
        prompt: String,
    },
    List,
    Remove {
        id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AllowlistAction {
    Add { id: String },
    Remove { id: String },
    List,
}

/// Register the outgoing `discord_send` tool in gray's plugin lock.
/// Matches `gray_plugin::lock::LockEntry` (explicit argv, no fake install).
pub fn register(config: &Value, config_path: &Path) -> Result<(), String> {
    use serde_json::{json, Value};
    let gray_home = config
        .get("gray_home")
        .and_then(Value::as_str)
        .unwrap_or("");
    let lock = Path::new(gray_home).join("plugins/lock.json");
    let mut data: Value = std::fs::read(&lock)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({"schema": 1, "plugins": {}}));
    if data.get("schema").and_then(Value::as_u64) != Some(1)
        || !data.get("plugins").is_some_and(Value::is_object)
    {
        return Err("Unsupported gray plugin lock; not changing it".to_string());
    }
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| "cannot locate plugin binary".to_string())?;
    let resolved = config_path
        .to_str()
        .map(str::to_string)
        .unwrap_or_else(|| config_path.to_string_lossy().into_owned());
    let argv = vec![
        Value::String(exe),
        Value::String("sidecar".to_string()),
        Value::String("--config".to_string()),
        Value::String(resolved),
    ];
    let previous = data
        .get("plugins")
        .and_then(|p| p.get("discord"))
        .and_then(|d| d.get("argv"))
        .cloned();
    if let Some(prev) = previous {
        if prev != Value::Array(argv.clone()) {
            return Err(
                "Another discord plugin is registered; refusing to overwrite it".to_string(),
            );
        }
    }
    let installed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    data["plugins"]["discord"] = json!({
        "ecosystem": "gray-native",
        "version": "0.1.0",
        "hash": "",
        "source": "https://github.com/vstaln/gray-discord-plugin",
        "argv": argv,
        "adapter_version": "1.1",
        "installed_at": installed_at,
        "scope": "user",
        "enabled": true
    });
    crate::config::atomic_json(&lock, &data)
}

/// Parse the service-install answer. Python parity (`cli.py`):
/// `input('Enable and start the background service now? [Y/n] ').strip().lower()`
/// in `('', 'y', 'yes')` — default yes.
pub fn install_confirmed(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// Dispatch a parsed subcommand. Arms without a landed task fail closed so
/// no half-wired command can run.
pub fn run(cmd: &Command, config_path: &Path) -> Result<(), String> {
    use serde_json::Value;
    match cmd {
        Command::Sidecar => crate::sidecar::serve(config_path),
        Command::Register => {
            let config: Value = crate::config::load_config(config_path)?;
            register(&config, config_path)
        }
        Command::Setup => {
            use crate::setup::Prompter;
            let mut io = crate::setup::Tty;
            // setup is async; drive it on a throwaway current-thread runtime
            // so `run` stays sync like the other CLI arms.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "cannot start setup runtime".to_string())?;
            // Gateway-event pairing wait lands in Task 9; until then the
            // wizard reports that pairing needs the running gateway.
            let done = rt.block_on(crate::setup::run(config_path, &mut io, &|_| {
                Err("Pairing needs the gateway; run setup after first connect".to_string())
            }))?;
            if done {
                let config: Value = crate::config::load_config(config_path)?;
                register(&config, config_path)?;
                println!("Outgoing tool registered with gray.");
                let answer = io.prompt("Enable and start the background service now? [Y/n] ")?;
                if install_confirmed(&answer) {
                    crate::service::install(config_path)?;
                    println!("Service enabled. Run gray discord status to check it.");
                    println!("For operation after logout: loginctl enable-linger \"$USER\"");
                } else {
                    println!(
                        "Run gray discord install when ready, or gray discord run in the foreground."
                    );
                }
            }
            Ok(())
        }
        Command::Run => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|_| "cannot start gateway runtime".to_string())?;
            rt.block_on(crate::gateway::run(config_path))
        }
        _ => Err("not yet implemented".to_string()),
    }
}
