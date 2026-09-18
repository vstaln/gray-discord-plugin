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
        Command::Status => crate::service::control(&["status", crate::service::NAME]),
        Command::Stop => crate::service::control(&["stop", crate::service::NAME]),
        Command::Restart => crate::service::control(&["restart", crate::service::NAME]),
        Command::Uninstall => {
            crate::service::uninstall()?;
            println!("Service removed. Private configuration and sessions retained; registered outgoing tool retained.");
            Ok(())
        }
        Command::Install => {
            crate::service::install(config_path)?;
            println!("Service enabled. To survive logout, enable user linger: loginctl enable-linger \"$USER\"");
            Ok(())
        }
        Command::Doctor => {
            let config = crate::config::load_config(config_path)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "cannot start runtime".to_string())?;
            rt.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    crate::doctor::doctor(&config),
                )
                .await
                .map_err(|_| "doctor timed out after 30s".to_string())?
            })
        }
        Command::Share {
            skill,
            context,
            plugin_argv,
            clear,
        } => {
            let mut config = crate::config::load_config(config_path)?;
            if *clear {
                config["shared_skills"] = Value::Array(Vec::new());
                config["shared_context"] = Value::Array(Vec::new());
                config["shared_plugins"] = Value::Array(Vec::new());
            }
            let mut skills: Vec<String> = config
                .get("shared_skills")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            for s in skill {
                if !skills.contains(s) {
                    skills.push(s.clone());
                }
            }
            config["shared_skills"] = Value::Array(skills.into_iter().map(Value::String).collect());

            let mut contexts: Vec<String> = config
                .get("shared_context")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            for c in context {
                if !contexts.contains(c) {
                    contexts.push(c.clone());
                }
            }
            config["shared_context"] =
                Value::Array(contexts.into_iter().map(Value::String).collect());

            let mut plugins: Vec<Value> = config
                .get("shared_plugins")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for p_str in plugin_argv {
                let parsed: Value = serde_json::from_str(p_str)
                    .map_err(|_| "Invalid plugin-argv JSON".to_string())?;
                plugins.push(parsed);
            }
            config["shared_plugins"] = Value::Array(plugins);

            let tmp_dir = std::env::temp_dir().join(format!("gray-cap-{}", uuid_hex()));
            std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("cannot create tempdir: {e}"))?;
            let res = crate::capabilities::prepare(&config, &tmp_dir);
            let _ = std::fs::remove_dir_all(&tmp_dir);
            res?;
            crate::config::save_config(config_path, &config)?;
            println!("Shared capabilities saved. Only select trusted code/non-secret context. Restart to apply.");
            Ok(())
        }
        Command::Limits {
            timeout_seconds,
            concurrency,
            max_requests,
        } => {
            let mut config = crate::config::load_config(config_path)?;
            if let Some(n) = timeout_seconds {
                config["timeout_seconds"] = Value::from(*n);
            }
            if let Some(n) = concurrency {
                config["concurrency"] = Value::from(*n);
            }
            if let Some(n) = max_requests {
                config["max_requests"] = Value::from(*n);
            }
            crate::config::save_config(config_path, &config)?;
            println!("Limits saved; restart the gateway to apply.");
            Ok(())
        }
        Command::Budget { action } => match action {
            BudgetAction::Set {
                daily_usd,
                turn_usd,
                input_per_million,
                output_per_million,
            } => {
                let mut config = crate::config::load_config(config_path)?;
                let gray_home = config
                    .get("gray_home")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "gray_home missing".to_string())?;
                let provider_bytes = std::fs::read(Path::new(gray_home).join("config.json"))
                    .map_err(|_| "gray provider configuration is missing".to_string())?;
                let provider: Value = serde_json::from_slice(&provider_bytes)
                    .map_err(|_| "Invalid gray config.json".to_string())?;
                let model = provider
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "model missing".to_string())?;

                let new_budget = serde_json::json!({
                    "model": model,
                    "daily_usd": daily_usd,
                    "turn_usd": turn_usd,
                    "input_per_million": input_per_million,
                    "output_per_million": output_per_million,
                });
                crate::budget::validate(&new_budget, model)?;
                config["budget"] = new_budget;
                crate::config::save_config(config_path, &config)?;
                println!("Budget saved for selected model; restart the gateway to apply.");
                Ok(())
            }
            BudgetAction::Status => {
                let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
                let b = crate::budget::Budget::new(&parent.join("budget.sqlite"))?;
                println!("Accounted/reserved micro-USD: {}", b.total());
                Ok(())
            }
        },
        Command::Schedule { action } => {
            let store = crate::gateway::open_store(config_path)?;
            match action {
                ScheduleAction::Add { every, prompt } => {
                    let job_id = uuid_hex();
                    store.schedule_add(&job_id, *every, prompt, crate::durable::now_secs())?;
                    println!("{job_id}");
                    Ok(())
                }
                ScheduleAction::List => {
                    for job in store.schedules()? {
                        println!("{} {} {}", job.id, job.interval, job.status);
                    }
                    Ok(())
                }
                ScheduleAction::Remove { id } => {
                    store.schedule_remove(id)?;
                    Ok(())
                }
            }
        }
        Command::Queue { action } => {
            let store = crate::gateway::open_store(config_path)?;
            match action {
                QueueAction::List => {
                    for item in store.items()? {
                        println!(
                            "{} {} {} {}",
                            item.0,
                            item.1,
                            item.2,
                            item.3.as_deref().unwrap_or("")
                        );
                    }
                    Ok(())
                }
                QueueAction::Cancel { id } => {
                    store.cancel(id)?;
                    Ok(())
                }
            }
        }
        Command::Allowlist { action } => {
            let mut config = crate::config::load_config(config_path)?;
            let mut allowed: Vec<String> = config
                .get("allowed_users")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            match action {
                AllowlistAction::Add { id } => {
                    if !crate::config::snowflake(&Value::String(id.clone())) {
                        return Err("Invalid snowflake ID".to_string());
                    }
                    if !allowed.contains(id) {
                        allowed.push(id.clone());
                        config["allowed_users"] =
                            Value::Array(allowed.into_iter().map(Value::String).collect());
                        crate::config::save_config(config_path, &config)?;
                    }
                    Ok(())
                }
                AllowlistAction::Remove { id } => {
                    if !crate::config::snowflake(&Value::String(id.clone())) {
                        return Err("Invalid snowflake ID".to_string());
                    }
                    allowed.retain(|x| x != id);
                    config["allowed_users"] =
                        Value::Array(allowed.into_iter().map(Value::String).collect());
                    crate::config::save_config(config_path, &config)?;
                    Ok(())
                }
                AllowlistAction::List => {
                    for id in allowed {
                        println!("{id}");
                    }
                    Ok(())
                }
            }
        }
    }
}

fn uuid_hex() -> String {
    let mut buf = [0u8; 16];
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut buf);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
