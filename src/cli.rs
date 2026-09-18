//! CLI tree: `gray-discord --config <path> <subcommand>`. Mirrors gray_discord/cli.py parser().
use clap::{Parser, Subcommand};
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

/// Dispatch a parsed subcommand. Full handlers land in their task; until then
/// every arm fails closed so no half-wired command can run.
pub fn run(cmd: &Command, _config_path: &Path) -> Result<(), String> {
    let _ = cmd;
    Err("not yet implemented".to_string())
}
