//! Port of gray_discord/durable.py: transactional inbox/outbox queue.
//! Generation and delivery are separate: a failed delivery retries the same
//! chunks, never the agent turn. Interrupted work is marked uncertain.
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct InboxItem {
    pub id: String,
    pub channel: String,
    pub conversation: String,
    pub prompt: String,
    pub state: String,
    pub created: f64,
    pub error: Option<String>,
    pub receipt: Option<String>,
    pub cancel: bool,
    pub interaction_token: Option<String>,
    pub app_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OutboxPart {
    pub id: String,
    pub part: i64,
    pub content: String,
    pub channel: String,
    pub attempts: i64,
    /// Slash followup routing (Task 9): `Some` when the row was enqueued from
    /// `/ask`. The production deliver closure sends these via the interaction
    /// webhook (`Rest::followup`) instead of the channel (`Rest::send`).
    pub interaction_token: Option<String>,
    /// Application id paired with `interaction_token` for webhook followups.
    pub app_id: Option<String>,
}

/// One row of the `queue list` view.
pub type QueueRow = (String, String, String, Option<String>);

#[derive(Debug, Clone)]
pub struct Schedule {
    pub id: String,
    pub interval: i64,
    pub prompt: String,
    pub next_at: f64,
    pub status: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS inbox (
    id TEXT PRIMARY KEY, channel TEXT NOT NULL, conversation TEXT NOT NULL,
    prompt TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued',
    created REAL NOT NULL, error TEXT, receipt TEXT, cancel INTEGER DEFAULT 0,
    interaction_token TEXT, app_id TEXT);
CREATE TABLE IF NOT EXISTS outbox (
    id TEXT NOT NULL, part INTEGER NOT NULL, content TEXT NOT NULL,
    message_id TEXT, attempts INTEGER NOT NULL DEFAULT 0,
    next_at REAL NOT NULL DEFAULT 0, error TEXT,
    PRIMARY KEY(id,part));
CREATE TABLE IF NOT EXISTS schedules (
    id TEXT PRIMARY KEY, interval INTEGER NOT NULL, prompt TEXT NOT NULL,
    next_at REAL NOT NULL, status TEXT NOT NULL DEFAULT 'scheduled');
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS pairings (
    code TEXT PRIMARY KEY, user_id TEXT NOT NULL, created REAL NOT NULL);
";

fn connect(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|_| "cannot create queue directory".to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
    }
    let conn = Connection::open(path).map_err(|_| "cannot open queue".to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "cannot open queue".to_string())?;
    conn.execute_batch("PRAGMA synchronous=FULL")
        .map_err(|_| "cannot open queue".to_string())?;
    conn.execute_batch(SCHEMA)
        .map_err(|_| "cannot open queue".to_string())?;
    // Migrate pre-slash databases that lack the followup columns.
    for col in ["interaction_token TEXT", "app_id TEXT"] {
        let name = col.split_whitespace().next().unwrap_or(col);
        let sql = format!("ALTER TABLE inbox ADD COLUMN {col}");
        match conn.execute_batch(&sql) {
            Ok(()) => {}
            Err(e) => {
                if !e.to_string().contains(name) {
                    return Err("cannot migrate queue".to_string());
                }
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(conn)
}

fn with_conn<T>(
    path: &Path,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    let conn = connect(path)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|_| "queue is busy".to_string())?;
    match f(&conn) {
        Ok(v) => {
            conn.execute_batch("COMMIT")
                .map_err(|_| "cannot commit queue".to_string())?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn row_item(r: &rusqlite::Row) -> Result<InboxItem, rusqlite::Error> {
    Ok(InboxItem {
        id: r.get("id")?,
        channel: r.get("channel")?,
        conversation: r.get("conversation")?,
        prompt: r.get("prompt")?,
        state: r.get("state")?,
        created: r.get("created")?,
        error: r.get("error")?,
        receipt: r.get("receipt")?,
        cancel: r.get::<_, i64>("cancel")? != 0,
        interaction_token: r.get("interaction_token")?,
        app_id: r.get("app_id")?,
    })
}

/// Transactional inbox/outbox/schedule store.
///
/// `Clone` is cheap (path only — every method opens a fresh connection), so
/// the gateway event loop and worker tasks can share one store handle.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: &Path) -> Result<Self, String> {
        connect(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn enqueue(
        &self,
        id: &str,
        channel: &str,
        prompt: &str,
        conversation: Option<&str>,
        capacity: u64,
    ) -> Result<bool, String> {
        if prompt.trim().is_empty() || prompt.len() > 32000 {
            return Err("Prompt must contain 1–32000 characters".to_string());
        }
        with_conn(&self.path, |db| {
            let dup: bool = db
                .query_row("SELECT 1 FROM inbox WHERE id=?1", params![id], |_| Ok(true))
                .optional_str()?
                .is_some();
            if dup {
                return Ok(false);
            }
            let pending: i64 = db
                .query_row(
                    "SELECT count(*) FROM inbox WHERE state IN ('queued','running','delivery')",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| "cannot enqueue".to_string())?;
            if pending >= capacity as i64 {
                return Err("Queue is full; message was not accepted".to_string());
            }
            let conv = conversation
                .map(str::to_string)
                .unwrap_or_else(|| format!("chat:{channel}"));
            let now = now_secs();
            db.execute(
                "INSERT INTO inbox(id,channel,conversation,prompt,created) VALUES(?1,?2,?3,?4,?5)",
                params![id, channel, conv, prompt, now],
            )
            .map_err(|_| "cannot enqueue".to_string())?;
            Ok(true)
        })
    }

    pub fn claim(&self) -> Result<Option<InboxItem>, String> {
        with_conn(&self.path, |db| {
            let item: Option<InboxItem> = db
                .query_row(
                    "SELECT * FROM inbox q WHERE state='queued' AND cancel=0 AND NOT EXISTS
                 (SELECT 1 FROM inbox r WHERE r.conversation=q.conversation AND r.state='running')
                 ORDER BY created,id LIMIT 1",
                    [],
                    row_item,
                )
                .optional_str()?;
            if let Some(ref it) = item {
                db.execute(
                    "UPDATE inbox SET state='running' WHERE id=?1",
                    params![it.id],
                )
                .map_err(|_| "cannot claim".to_string())?;
            }
            Ok(item)
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<InboxItem>, String> {
        with_conn(&self.path, |db| {
            db.query_row("SELECT * FROM inbox WHERE id=?1", params![id], row_item)
                .optional_str()
        })
    }

    pub fn items(&self) -> Result<Vec<QueueRow>, String> {
        with_conn(&self.path, |db| {
            let mut stmt = db
                .prepare("SELECT id,channel,state,error FROM inbox ORDER BY created DESC LIMIT 100")
                .map_err(|_| "cannot list queue".to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|_| "cannot list queue".to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| "cannot list queue".to_string())
        })
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        with_conn(&self.path, |db| {
            let n = db.execute(
                "UPDATE inbox SET cancel=1, state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END
                 WHERE id=?1 AND state IN ('queued','running')",
                params![id],
            ).map_err(|_| "cannot cancel".to_string())?;
            if n == 0 {
                return Err("No queued/running item with that ID".to_string());
            }
            Ok(())
        })
    }

    pub fn complete(&self, id: &str, text: &str, receipt: &Value) -> Result<(), String> {
        if text.trim().is_empty() || text.len() > 200000 {
            return Err("Completed answer must contain 1–200000 characters".to_string());
        }
        let chunks = crate::text::split_message(text, 2000).map_err(|e| e.to_string())?;
        let dropped_chars: usize = if chunks.len() > 8 {
            chunks[7..].iter().map(|c| c.len()).sum()
        } else {
            0
        };
        let capped = cap_chunks(chunks, dropped_chars);
        let receipt_text =
            serde_json::to_string(receipt).map_err(|_| "cannot encode receipt".to_string())?;
        with_conn(&self.path, |db| {
            let n = db
                .execute(
                    "UPDATE inbox SET state='delivery',receipt=?1 WHERE id=?2 AND state='running'",
                    params![receipt_text, id],
                )
                .map_err(|_| "cannot complete".to_string())?;
            if n == 0 {
                return Err("Item is not running".to_string());
            }
            for (part, chunk) in capped.iter().enumerate() {
                db.execute(
                    "INSERT INTO outbox(id,part,content) VALUES(?1,?2,?3)",
                    params![id, part as i64, chunk],
                )
                .map_err(|_| "cannot complete".to_string())?;
            }
            Ok(())
        })
    }

    pub fn fail(&self, id: &str, code: &str) -> Result<(), String> {
        let code = match code {
            "interrupted" | "cancelled" | "timeout" | "agent_failed" | "budget_blocked" => code,
            _ => "agent_failed",
        };
        let notice =
            format!("Turn {id}: {code}. Actions may already have happened; no automatic retry.");
        with_conn(&self.path, |db| {
            db.execute(
                "UPDATE inbox SET state='uncertain',error=?1 WHERE id=?2 AND state='running'",
                params![code, id],
            )
            .map_err(|_| "cannot fail".to_string())?;
            db.execute(
                "INSERT OR IGNORE INTO outbox(id,part,content) VALUES(?1,0,?2)",
                params![id, notice],
            )
            .map_err(|_| "cannot fail".to_string())?;
            Ok(())
        })
    }

    pub fn recover(&self) -> Result<(), String> {
        let ids: Vec<String> = with_conn(&self.path, |db| {
            let mut stmt = db
                .prepare("SELECT id FROM inbox WHERE state='running'")
                .map_err(|_| "cannot recover".to_string())?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(|_| "cannot recover".to_string())?;
            rows.collect::<Result<Vec<String>, _>>()
                .map_err(|_| "cannot recover".to_string())
        })?;
        for id in ids {
            self.fail(&id, "interrupted")?;
        }
        Ok(())
    }

    pub fn next_delivery(&self, now: f64) -> Result<Option<OutboxPart>, String> {
        with_conn(&self.path, |db| {
            db.query_row(
                "SELECT o.id,o.part,o.content,i.channel,o.attempts,i.interaction_token,i.app_id FROM outbox o JOIN inbox i ON i.id=o.id
                 WHERE message_id IS NULL AND next_at<=?1 AND NOT EXISTS
                 (SELECT 1 FROM outbox p WHERE p.id=o.id AND p.part<o.part AND p.message_id IS NULL)
                 ORDER BY i.created,o.part LIMIT 1",
                params![now],
                |r| Ok(OutboxPart { id: r.get(0)?, part: r.get(1)?, content: r.get(2)?, channel: r.get(3)?, attempts: r.get(4)?, interaction_token: r.get(5)?, app_id: r.get(6)? }),
            ).optional_str()
        })
    }

    pub fn ack(&self, id: &str, part: i64, message_id: &str) -> Result<(), String> {
        with_conn(&self.path, |db| {
            db.execute(
                "UPDATE outbox SET message_id=?1,error=NULL WHERE id=?2 AND part=?3",
                params![message_id, id, part],
            )
            .map_err(|_| "cannot ack".to_string())?;
            let pending: bool = db
                .query_row(
                    "SELECT 1 FROM outbox WHERE id=?1 AND message_id IS NULL",
                    params![id],
                    |_| Ok(true),
                )
                .optional_str()?
                .is_some();
            if !pending {
                db.execute(
                    "UPDATE inbox SET state='sent' WHERE id=?1 AND state='delivery'",
                    params![id],
                )
                .map_err(|_| "cannot ack".to_string())?;
            }
            Ok(())
        })
    }

    pub fn delivery_failed(&self, part: &OutboxPart, code: &str, now: f64) -> Result<(), String> {
        let shift = (part.attempts + 1).clamp(0, 12) as u32;
        let delay = (2u64.saturating_pow(shift)).min(3600) as f64;
        with_conn(&self.path, |db| {
            db.execute(
                "UPDATE outbox SET attempts=attempts+1,next_at=?1,error=?2 WHERE id=?3 AND part=?4",
                params![now + delay, code, part.id, part.part],
            )
            .map_err(|_| "cannot record delivery failure".to_string())?;
            Ok(())
        })
    }

    pub fn schedule_add(
        &self,
        id: &str,
        interval: u64,
        prompt: &str,
        now: f64,
    ) -> Result<(), String> {
        if interval < 60 || prompt.trim().is_empty() || prompt.len() > 32000 {
            return Err("Interval must be >=60s and prompt 1–32000 characters".to_string());
        }
        with_conn(&self.path, |db| {
            db.execute(
                "INSERT INTO schedules(id,interval,prompt,next_at) VALUES(?1,?2,?3,?4)",
                params![id, interval as i64, prompt, now + interval as f64],
            )
            .map_err(|_| "cannot add schedule".to_string())?;
            Ok(())
        })
    }

    pub fn schedules(&self) -> Result<Vec<Schedule>, String> {
        with_conn(&self.path, |db| {
            let mut stmt = db
                .prepare("SELECT id,interval,prompt,next_at,status FROM schedules ORDER BY id")
                .map_err(|_| "cannot list schedules".to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(Schedule {
                        id: r.get(0)?,
                        interval: r.get(1)?,
                        prompt: r.get(2)?,
                        next_at: r.get(3)?,
                        status: r.get(4)?,
                    })
                })
                .map_err(|_| "cannot list schedules".to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| "cannot list schedules".to_string())
        })
    }

    pub fn schedule_remove(&self, id: &str) -> Result<(), String> {
        with_conn(&self.path, |db| {
            let n = db
                .execute("DELETE FROM schedules WHERE id=?1", params![id])
                .map_err(|_| "cannot remove schedule".to_string())?;
            if n == 0 {
                return Err("Schedule not found".to_string());
            }
            Ok(())
        })
    }

    /// A pending pairing code for this user, if one is already waiting —
    /// repeat "hi" from an unknown DMer reuses it instead of piling codes up.
    pub fn pairing_code_for(&self, user_id: &str) -> Result<Option<String>, String> {
        with_conn(&self.path, |db| {
            db.query_row(
                "SELECT code FROM pairings WHERE user_id=?1 ORDER BY created DESC LIMIT 1",
                params![user_id],
                |r| r.get(0),
            )
            .optional_str()
            .map_err(|_| "cannot read pairings".to_string())
        })
    }

    pub fn pairing_insert(&self, code: &str, user_id: &str) -> Result<(), String> {
        with_conn(&self.path, |db| {
            db.execute(
                "INSERT OR REPLACE INTO pairings(code,user_id,created) VALUES(?1,?2,?3)",
                params![code, user_id, crate::durable::now_secs()],
            )
            .map(|_| ())
            .map_err(|_| "cannot store pairing".to_string())
        })
    }

    /// Consume a code exactly once. A replay finds nothing.
    pub fn pairing_take(&self, code: &str) -> Result<Option<String>, String> {
        with_conn(&self.path, |db| {
            let found = db
                .query_row(
                    "SELECT user_id FROM pairings WHERE code=?1",
                    params![code],
                    |r| r.get::<_, String>(0),
                )
                .optional_str()
                .map_err(|_| "cannot read pairings".to_string())?;
            if found.is_some() {
                db.execute("DELETE FROM pairings WHERE code=?1", params![code])
                    .map_err(|_| "cannot consume pairing".to_string())?;
            }
            Ok(found)
        })
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>, String> {
        with_conn(&self.path, |db| {
            db.query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
                r.get(0)
            })
            .optional_str()
        })
    }

    /// Upsert a `meta` key.
    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), String> {
        with_conn(&self.path, |db| {
            db.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|_| "cannot store meta".to_string())
        })
    }

    /// Attach slash followup routing to an enqueued row (`/ask` path).
    pub fn set_interaction(&self, id: &str, token: &str, app_id: &str) -> Result<(), String> {
        with_conn(&self.path, |db| {
            db.execute(
                "UPDATE inbox SET interaction_token=?1,app_id=?2 WHERE id=?3",
                params![token, app_id, id],
            )
            .map(|_| ())
            .map_err(|_| "cannot store interaction".to_string())
        })
    }

    /// Flag the running row of `conversation` for cancellation (`/stop`).
    /// Returns true when a running turn was flagged.
    pub fn cancel_conversation(&self, conversation: &str) -> Result<bool, String> {
        with_conn(&self.path, |db| {
            let n = db
                .execute(
                    "UPDATE inbox SET cancel=1 WHERE conversation=?1 AND state='running'",
                    params![conversation],
                )
                .map_err(|_| "cannot cancel".to_string())?;
            Ok(n > 0)
        })
    }

    /// Queued + running + delivery rows (`/status` queue depth).
    pub fn pending_count(&self) -> Result<i64, String> {
        with_conn(&self.path, |db| {
            db.query_row(
                "SELECT count(*) FROM inbox WHERE state IN ('queued','running','delivery')",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "cannot count queue".to_string())
        })
    }

    pub fn enqueue_due(&self, channel: &str, now: f64) -> Result<(), String> {
        with_conn(&self.path, |db| {
            let mut stmt = db
                .prepare("SELECT id,interval,prompt,next_at FROM schedules WHERE next_at<=?1")
                .map_err(|_| "cannot enqueue due".to_string())?;
            let due: Vec<(String, i64, String, f64)> = stmt
                .query_map(params![now], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(|_| "cannot enqueue due".to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "cannot enqueue due".to_string())?;
            for (job_id, interval, prompt, next_at) in due {
                let inbox_id = format!("job:{job_id}:{next_at}");
                let conv = format!("job:{job_id}");
                db.execute(
                    "INSERT OR IGNORE INTO inbox(id,channel,conversation,prompt,created) VALUES(?1,?2,?3,?4,?5)",
                    params![inbox_id, channel, conv, prompt, now],
                ).map_err(|_| "cannot enqueue due".to_string())?;
                db.execute(
                    "UPDATE schedules SET next_at=?1,status='queued' WHERE id=?2",
                    params![now + interval as f64, job_id],
                )
                .map_err(|_| "cannot enqueue due".to_string())?;
            }
            Ok(())
        })
    }

    pub fn migrate_jobs(&self, path: &Path) -> Result<(), String> {
        with_conn(&self.path, |db| {
            let done: bool = db
                .query_row("SELECT 1 FROM meta WHERE key='jobs_migrated'", [], |_| {
                    Ok(true)
                })
                .optional_str()?
                .is_some();
            if done {
                return Ok(());
            }
            if path.exists() {
                let text = std::fs::read_to_string(path)
                    .map_err(|_| "cannot read legacy schedules".to_string())?;
                let jobs: Value = serde_json::from_str(&text)
                    .map_err(|_| "Invalid schedules file".to_string())?;
                let arr = jobs
                    .as_array()
                    .ok_or_else(|| "Invalid schedules file".to_string())?;
                for job in arr {
                    let interval = job.get("interval").and_then(Value::as_i64).unwrap_or(0);
                    if interval < 60 {
                        return Err("Invalid legacy schedule interval".to_string());
                    }
                    let id = job.get("id").and_then(Value::as_str).unwrap_or("");
                    let prompt = job.get("prompt").and_then(Value::as_str).unwrap_or("");
                    let next_at = job.get("next_at").and_then(Value::as_f64).unwrap_or(0.0);
                    let status = job
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("scheduled");
                    db.execute(
                        "INSERT OR IGNORE INTO schedules(id,interval,prompt,next_at,status) VALUES(?1,?2,?3,?4,?5)",
                        params![id, interval, prompt, next_at, status],
                    ).map_err(|_| "cannot migrate schedules".to_string())?;
                }
            }
            db.execute("INSERT INTO meta VALUES('jobs_migrated','1')", [])
                .map_err(|_| "cannot migrate schedules".to_string())?;
            Ok(())
        })
    }

    /// Delete terminal inbox rows (sent/cancelled/uncertain) older than
    /// `retention_secs`, plus their outbox parts. Hermes recovery parity.
    pub fn prune_terminal(&self, retention_secs: u64, now: f64) -> Result<usize, String> {
        with_conn(&self.path, |db| {
            let cutoff = now - retention_secs as f64;
            let mut stmt = db.prepare("SELECT id FROM inbox WHERE state IN ('sent','cancelled','uncertain') AND created < ?1")
                .map_err(|_| "cannot prune".to_string())?;
            let ids: Vec<String> = stmt
                .query_map(params![cutoff], |r| r.get(0))
                .map_err(|_| "cannot prune".to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "cannot prune".to_string())?;
            for id in &ids {
                db.execute("DELETE FROM outbox WHERE id=?1", params![id])
                    .map_err(|_| "cannot prune".to_string())?;
                db.execute("DELETE FROM inbox WHERE id=?1", params![id])
                    .map_err(|_| "cannot prune".to_string())?;
            }
            Ok(ids.len())
        })
    }
}

/// Hermes anti-flood (MAX_SPLIT_MESSAGES = 8): keep the first 7 chunks,
/// replace the rest with a notice carrying the dropped character count.
pub(crate) fn cap_chunks(chunks: Vec<String>, dropped_chars: usize) -> Vec<String> {
    const MAX: usize = 8;
    if chunks.len() <= MAX {
        return chunks;
    }
    let mut kept = chunks.into_iter().take(MAX - 1).collect::<Vec<_>>();
    kept.push(format!(
        "… (truncated, {dropped_chars} more characters not sent)"
    ));
    kept
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

trait OptionalStr<T> {
    fn optional_str(self) -> Result<Option<T>, String>;
}
impl<T> OptionalStr<T> for Result<T, rusqlite::Error> {
    fn optional_str(self) -> Result<Option<T>, String> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(_) => Err("queue query failed".to_string()),
        }
    }
}
