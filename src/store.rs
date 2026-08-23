use std::{
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::model::{
    AutomationListArgs, AutomationRunsArgs, AutomationState, AutomationSummary, AutomationView,
    RunState, RunView,
};

const SCHEMA_VERSION: u32 = 2;
const ACTOR_QUEUE_CAPACITY: usize = 128;
const MAX_AUTOMATIONS_PER_TASK: u64 = 64;
const MAX_AUTOMATIONS_GLOBAL: u64 = 1_024;
const MAX_RUNS_PER_TASK: u64 = 2_000;
const MAX_RUNS_GLOBAL: u64 = 20_000;
const MAX_EVENTS_PER_TASK: u64 = 5_000;
const MAX_EVENTS_GLOBAL: u64 = 25_000;
const MAX_REQUEST_KEYS: u64 = 4_096;
const MAX_TASK_GRANTS: u64 = 4_096;
const TERMINAL_RUN_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1_000;
const EVENT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

const TERMINAL_RUN_STATES_SQL: &str = r#"'"succeeded"','"turn_failed"','"delivery_failed"','"interrupted"',
       '"authority_changed"','"remote_ownership_violation"','"task_closed"',
       '"cancelled"','"coalesced"','"superseded"','"expired"','"skipped"'"#;

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Job>,
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAutomation {
    pub view: AutomationView,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRun {
    pub view: RunView,
    pub client_user_message_id: String,
    pub input_digest: String,
    pub remote_submission_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreStats {
    pub schema_version: u32,
    pub healthy: bool,
    pub active_automations: u64,
    pub blocked_runs: u64,
    pub uncertain_runs: u64,
    pub ownership_violations: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path.parent().context("database path has no parent")?;
        prepare_private_directory(parent)?;

        let (tx, mut rx) = mpsc::channel::<Job>(ACTOR_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let actor_path = path.clone();
        thread::Builder::new()
            .name("threadbeat-sqlite".to_owned())
            .spawn(move || {
                let result = open_connection(&actor_path);
                match result {
                    Ok(mut connection) => {
                        let _ = ready_tx.send(Ok(()));
                        while let Some(job) = rx.blocking_recv() {
                            job(&mut connection);
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .context("failed to start SQLite owner thread")?;

        ready_rx
            .recv()
            .context("SQLite owner exited before initialization")??;
        Ok(Self { tx, path })
    }

    async fn call<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |connection| {
                let _ = response_tx.send(operation(connection));
            }))
            .await
            .map_err(|_| anyhow!("SQLite owner is unavailable"))?;
        response_rx.await.context("SQLite owner dropped response")?
    }

    pub async fn integrity_check(&self) -> Result<bool> {
        self.call(|connection| {
            let result: String =
                connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            Ok(result == "ok")
        })
        .await
    }

    pub async fn create_automation(&self, record: StoredAutomation) -> Result<()> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            enforce_automation_quota(&transaction, &record.view.target.thread_id)?;
            let next_due = record.view.next_nominal_due_at.map(|value| value.timestamp_millis());
            let view_json = serde_json::to_string(&record.view)?;
            transaction.execute(
                "INSERT INTO automations
                 (id, thread_id, revision, state, name, prompt, view_json, next_due_ms, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.view.id,
                    record.view.target.thread_id,
                    i64_from_u64(record.view.revision)?,
                    enum_json(&record.view.state)?,
                    record.view.name,
                    record.prompt,
                    view_json,
                    next_due,
                    record.view.created_at.timestamp_millis(),
                    record.view.updated_at.timestamp_millis(),
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    /// Insert a deterministic automation once. A false result means the ID
    /// already existed; callers must load and verify the scoped record.
    pub async fn create_automation_if_absent(&self, record: StoredAutomation) -> Result<bool> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM automations WHERE id=?1)",
                [&record.view.id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(false);
            }
            enforce_automation_quota(&transaction, &record.view.target.thread_id)?;
            let next_due = record
                .view
                .next_nominal_due_at
                .map(|value| value.timestamp_millis());
            let view_json = serde_json::to_string(&record.view)?;
            transaction.execute(
                "INSERT INTO automations
                 (id, thread_id, revision, state, name, prompt, view_json, next_due_ms, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.view.id,
                    record.view.target.thread_id,
                    i64_from_u64(record.view.revision)?,
                    enum_json(&record.view.state)?,
                    record.view.name,
                    record.prompt,
                    view_json,
                    next_due,
                    record.view.created_at.timestamp_millis(),
                    record.view.updated_at.timestamp_millis(),
                ],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
    }

    pub async fn replace_automation(
        &self,
        thread_id: String,
        expected_revision: u64,
        record: StoredAutomation,
    ) -> Result<bool> {
        self.call(move |connection| {
            let next_due = record
                .view
                .next_nominal_due_at
                .map(|value| value.timestamp_millis());
            let view_json = serde_json::to_string(&record.view)?;
            let changed = connection.execute(
                "UPDATE automations SET revision=?1, state=?2, name=?3, prompt=?4,
                 view_json=?5, next_due_ms=?6, updated_ms=?7
                 WHERE id=?8 AND thread_id=?9 AND revision=?10",
                params![
                    i64_from_u64(record.view.revision)?,
                    enum_json(&record.view.state)?,
                    record.view.name,
                    record.prompt,
                    view_json,
                    next_due,
                    record.view.updated_at.timestamp_millis(),
                    record.view.id,
                    thread_id,
                    i64_from_u64(expected_revision)?,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    pub async fn get_automation(
        &self,
        thread_id: String,
        automation_id: String,
    ) -> Result<Option<StoredAutomation>> {
        self.call(move |connection| {
            connection
                .query_row(
                    "SELECT view_json, prompt FROM automations WHERE id=?1 AND thread_id=?2",
                    params![automation_id, thread_id],
                    decode_automation_row,
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn list_automations(
        &self,
        thread_id: String,
        args: AutomationListArgs,
    ) -> Result<(Vec<AutomationSummary>, Option<String>)> {
        self.call(move |connection| {
            let mut statement = connection.prepare(
                "SELECT a.view_json,
                        (SELECT r.view_json FROM runs r WHERE r.automation_id=a.id
                         ORDER BY r.nominal_due_ms DESC, r.id DESC LIMIT 1)
                 FROM automations a WHERE a.thread_id=?1
                 ORDER BY a.updated_ms DESC, a.id DESC",
            )?;
            let rows = statement.query_map([thread_id], |row| {
                let view_json: String = row.get(0)?;
                let last_run_json: Option<String> = row.get(1)?;
                Ok((view_json, last_run_json))
            })?;

            let offset = parse_cursor(args.cursor.as_deref())?;
            let limit = usize::from(args.limit.unwrap_or(25).clamp(1, 100));
            let name_filter = args
                .name_contains
                .as_ref()
                .map(|value| value.to_lowercase());
            let state_filter = args.states.as_ref();
            let mut matching = Vec::new();
            for row in rows {
                let (view_json, last_run_json) = row?;
                let view: AutomationView = serde_json::from_str(&view_json)?;
                if !args.include_deleted && view.state == AutomationState::Deleted {
                    continue;
                }
                if state_filter.is_some_and(|states| !states.contains(&view.state)) {
                    continue;
                }
                if name_filter
                    .as_ref()
                    .is_some_and(|needle| !view.name.to_lowercase().contains(needle))
                {
                    continue;
                }
                let last_run_state = last_run_json
                    .map(|json| serde_json::from_str::<RunView>(&json))
                    .transpose()?
                    .map(|run| run.state);
                matching.push(AutomationSummary {
                    automation: view,
                    last_run_state,
                });
            }
            let page = matching
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            let next =
                (offset + page.len() < matching.len()).then(|| (offset + page.len()).to_string());
            Ok((page, next))
        })
        .await
    }

    pub async fn due_automations(
        &self,
        through: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<StoredAutomation>> {
        self.call(move |connection| {
            let mut statement = connection.prepare(
                "SELECT view_json, prompt FROM automations
                 WHERE state='\"active\"' AND next_due_ms IS NOT NULL AND next_due_ms<=?1
                 ORDER BY next_due_ms ASC, id ASC LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![
                    through.timestamp_millis(),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                decode_automation_row,
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn create_run(&self, record: StoredRun) -> Result<bool> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            let duplicate = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 OR logical_occurrence_id=?2)",
                params![record.view.id, record.view.logical_occurrence_id],
                |row| row.get::<_, bool>(0),
            )?;
            if duplicate {
                return Ok(false);
            }
            let thread_id: Option<String> = transaction
                .query_row(
                    "SELECT thread_id FROM automations WHERE id=?1",
                    [&record.view.automation_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(thread_id) = thread_id else {
                return Ok(false);
            };
            prune_and_enforce_run_quotas(&transaction, &thread_id, Utc::now().timestamp_millis())?;
            let view_json = serde_json::to_string(&record.view)?;
            let changed = transaction.execute(
                "INSERT OR IGNORE INTO runs
                 (id, thread_id, automation_id, revision, state, logical_occurrence_id,
                  nominal_due_ms, client_user_message_id, input_digest, remote_submission_id,
                  view_json, updated_ms)
                 SELECT ?1, thread_id, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
                 FROM automations WHERE id=?2",
                params![
                    record.view.id,
                    record.view.automation_id,
                    i64_from_u64(record.view.revision)?,
                    enum_json(&record.view.state)?,
                    record.view.logical_occurrence_id,
                    record.view.nominal_due_at.timestamp_millis(),
                    record.client_user_message_id,
                    record.input_digest,
                    record.remote_submission_id,
                    view_json,
                    Utc::now().timestamp_millis(),
                ],
            )?;
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    pub async fn replace_run(
        &self,
        thread_id: String,
        expected_revision: u64,
        record: StoredRun,
    ) -> Result<bool> {
        self.call(move |connection| {
            let view_json = serde_json::to_string(&record.view)?;
            let changed = connection.execute(
                "UPDATE runs SET revision=?1, state=?2, remote_submission_id=?3,
                 view_json=?4, updated_ms=?5
                 WHERE id=?6 AND thread_id=?7 AND revision=?8",
                params![
                    i64_from_u64(record.view.revision)?,
                    enum_json(&record.view.state)?,
                    record.remote_submission_id,
                    view_json,
                    Utc::now().timestamp_millis(),
                    record.view.id,
                    thread_id,
                    i64_from_u64(expected_revision)?,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    pub async fn get_run(&self, thread_id: String, run_id: String) -> Result<Option<StoredRun>> {
        self.call(move |connection| {
            connection
                .query_row(
                    "SELECT view_json, client_user_message_id, input_digest, remote_submission_id
                     FROM runs WHERE id=?1 AND thread_id=?2",
                    params![run_id, thread_id],
                    decode_run_row,
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn list_runs(
        &self,
        thread_id: String,
        args: AutomationRunsArgs,
    ) -> Result<(Vec<RunView>, Option<String>)> {
        self.call(move |connection| {
            let mut statement = connection.prepare(
                "SELECT view_json FROM runs WHERE thread_id=?1
                 ORDER BY nominal_due_ms DESC, id DESC",
            )?;
            let rows = statement.query_map([thread_id], |row| row.get::<_, String>(0))?;
            let offset = parse_cursor(args.cursor.as_deref())?;
            let limit = usize::from(args.limit.unwrap_or(25).clamp(1, 100));
            let mut matching = Vec::new();
            for row in rows {
                let mut run: RunView = serde_json::from_str(&row?)?;
                if args
                    .automation_id
                    .as_ref()
                    .is_some_and(|id| id != &run.automation_id)
                    || args.run_id.as_ref().is_some_and(|id| id != &run.id)
                    || args
                        .states
                        .as_ref()
                        .is_some_and(|states| !states.contains(&run.state))
                    || args.from.is_some_and(|from| run.nominal_due_at < from)
                    || args.to.is_some_and(|to| run.nominal_due_at > to)
                {
                    continue;
                }
                if !args.include_attempts {
                    run.attempts = None;
                }
                matching.push(run);
            }
            let page = matching
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            let next =
                (offset + page.len() < matching.len()).then(|| (offset + page.len()).to_string());
            Ok((page, next))
        })
        .await
    }

    pub async fn latest_run_for_automation(
        &self,
        thread_id: String,
        automation_id: String,
    ) -> Result<Option<RunView>> {
        self.call(move |connection| {
            connection
                .query_row(
                    "SELECT view_json FROM runs WHERE thread_id=?1 AND automation_id=?2
                     ORDER BY nominal_due_ms DESC, id DESC LIMIT 1",
                    params![thread_id, automation_id],
                    |row| {
                        let json: String = row.get(0)?;
                        from_json_column(&json)
                    },
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn task_has_nonterminal_run(&self, thread_id: String) -> Result<bool> {
        self.call(move |connection| {
            let mut statement = connection.prepare("SELECT state FROM runs WHERE thread_id=?1")?;
            let rows = statement.query_map([thread_id], |row| row.get::<_, String>(0))?;
            for state in rows {
                let state: RunState = serde_json::from_str(&state?)?;
                if !state.is_terminal() {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
    }

    pub async fn idempotency_get(&self, scope_hash: String) -> Result<Option<Value>> {
        self.call(move |connection| {
            connection.execute(
                "DELETE FROM request_keys WHERE expires_ms < ?1",
                [Utc::now().timestamp_millis()],
            )?;
            connection
                .query_row(
                    "SELECT response_json FROM request_keys WHERE scope_hash=?1",
                    [scope_hash],
                    |row| {
                        let json: String = row.get(0)?;
                        from_json_column(&json)
                    },
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    /// Atomically reserve a request key before its mutation. Returns the
    /// existing envelope when another attempt already reserved the scope.
    pub async fn idempotency_reserve(
        &self,
        scope_hash: String,
        pending: Value,
        expires_at: DateTime<Utc>,
    ) -> Result<Option<Value>> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            let now_ms = Utc::now().timestamp_millis();
            transaction.execute("DELETE FROM request_keys WHERE expires_ms < ?1", [now_ms])?;
            let existing = transaction
                .query_row(
                    "SELECT response_json FROM request_keys WHERE scope_hash=?1",
                    [&scope_hash],
                    |row| {
                        let json: String = row.get(0)?;
                        from_json_column(&json)
                    },
                )
                .optional()?;
            if existing.is_some() {
                transaction.commit()?;
                return Ok(existing);
            }
            if count(&transaction, "SELECT COUNT(*) FROM request_keys")? >= MAX_REQUEST_KEYS {
                bail!("quota exceeded: request keys (limit {MAX_REQUEST_KEYS})");
            }
            transaction.execute(
                "INSERT INTO request_keys(scope_hash, response_json, expires_ms)
                 VALUES (?1, ?2, ?3)",
                params![
                    scope_hash,
                    serde_json::to_string(&pending)?,
                    expires_at.timestamp_millis()
                ],
            )?;
            transaction.commit()?;
            Ok(None)
        })
        .await
    }

    /// Complete only the exact pending reservation supplied by the caller.
    /// Returns false if the reservation disappeared or changed.
    pub async fn idempotency_complete(
        &self,
        scope_hash: String,
        expected_pending: Value,
        response: Value,
        expires_at: DateTime<Utc>,
    ) -> Result<bool> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            let now_ms = Utc::now().timestamp_millis();
            transaction.execute("DELETE FROM request_keys WHERE expires_ms < ?1", [now_ms])?;
            let expected_json = serde_json::to_string(&expected_pending)?;
            let changed = transaction.execute(
                "UPDATE request_keys SET response_json=?1, expires_ms=?2
                 WHERE scope_hash=?3 AND response_json=?4",
                params![
                    serde_json::to_string(&response)?,
                    expires_at.timestamp_millis(),
                    scope_hash,
                    expected_json,
                ],
            )?;
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    pub async fn authorize_task(&self, thread_id: String, expires_at: DateTime<Utc>) -> Result<()> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            let now_ms = Utc::now().timestamp_millis();
            transaction.execute(
                "DELETE FROM task_grants WHERE expires_ms < ?1 OR remaining_uses <= 0",
                [now_ms],
            )?;
            let already_present: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_grants WHERE thread_id=?1)",
                [&thread_id],
                |row| row.get(0),
            )?;
            if !already_present
                && count(&transaction, "SELECT COUNT(*) FROM task_grants")? >= MAX_TASK_GRANTS
            {
                bail!("quota exceeded: task grants (limit {MAX_TASK_GRANTS})");
            }
            transaction.execute(
                "INSERT INTO task_grants(thread_id, expires_ms, granted_ms, remaining_uses)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(thread_id) DO UPDATE SET expires_ms=excluded.expires_ms,
                 granted_ms=excluded.granted_ms, remaining_uses=1",
                params![thread_id, expires_at.timestamp_millis(), now_ms],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn task_authorized(&self, thread_id: String, now: DateTime<Utc>) -> Result<bool> {
        self.call(move |connection| {
            let grant: Option<(i64, i64)> = connection
                .query_row(
                    "SELECT expires_ms, remaining_uses FROM task_grants WHERE thread_id=?1",
                    [thread_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            Ok(grant.is_some_and(|(expires_ms, remaining_uses)| {
                expires_ms >= now.timestamp_millis() && remaining_uses > 0
            }))
        })
        .await
    }

    pub async fn consume_task_authorization(
        &self,
        thread_id: String,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            let changed = transaction.execute(
                "UPDATE task_grants SET remaining_uses=remaining_uses-1
                 WHERE thread_id=?1 AND expires_ms>=?2 AND remaining_uses>0",
                params![thread_id, now.timestamp_millis()],
            )?;
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    pub async fn stats(&self) -> Result<StoreStats> {
        let path = self.path.clone();
        self.call(move |connection| {
            let healthy: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            let active_automations = count(connection, "SELECT COUNT(*) FROM automations WHERE state='\"active\"'")?;
            let blocked_runs = count(
                connection,
                "SELECT COUNT(*) FROM runs WHERE state IN ('\"waiting_approval\"','\"waiting_user_input\"','\"interrupted\"','\"authority_changed\"')",
            )?;
            let uncertain_runs = count(connection, "SELECT COUNT(*) FROM runs WHERE state='\"delivery_uncertain\"'")?;
            let ownership_violations = count(connection, "SELECT COUNT(*) FROM runs WHERE state='\"remote_ownership_violation\"'")?;
            Ok(StoreStats {
                schema_version: SCHEMA_VERSION,
                healthy: healthy == "ok",
                active_automations,
                blocked_runs,
                uncertain_runs,
                ownership_violations,
                database_bytes: file_len(&path),
                wal_bytes: file_len(Path::new(&format!("{}-wal", path.display()))),
            })
        })
        .await
    }

    pub async fn append_event(&self, thread_id: String, kind: String, data: Value) -> Result<()> {
        self.call(move |connection| {
            let mut transaction = connection.unchecked_transaction()?;
            transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
            prune_events(
                &transaction,
                &thread_id,
                Utc::now().timestamp_millis(),
            )?;
            transaction.execute(
                "INSERT INTO events(thread_id, kind, data_json, created_ms) VALUES (?1, ?2, ?3, ?4)",
                params![thread_id, kind, serde_json::to_string(&data)?, Utc::now().timestamp_millis()],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection =
        Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         PRAGMA trusted_schema=OFF;
         PRAGMA temp_store=MEMORY;",
    )?;
    migrate(&connection)?;
    set_private_file(path)?;
    Ok(connection)
}

fn migrate(connection: &Connection) -> Result<()> {
    let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!("database quick_check failed before migration");
    }
    let mut transaction = connection.unchecked_transaction()?;
    transaction.set_drop_behavior(rusqlite::DropBehavior::Rollback);
    let has_meta: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
        [],
        |row| row.get(0),
    )?;
    if has_meta {
        let version_text: Option<String> = transaction
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let version_text = version_text.context("database schema_version is missing")?;
        let version = version_text
            .parse::<u32>()
            .context("database schema_version is invalid")?;
        if version > SCHEMA_VERSION {
            bail!(
                "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
            );
        }
        match version {
            1 => migrate_v1_to_v2(&transaction)?,
            SCHEMA_VERSION => validate_schema_v2(&transaction)?,
            _ => bail!("unsupported database schema version {version}"),
        }
    } else {
        let application_tables: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if application_tables != 0 {
            bail!("refusing to initialize an unversioned non-empty database");
        }
        create_schema_v2(&transaction)?;
        transaction.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
    }
    transaction.commit()?;
    let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!("database quick_check failed after migration");
    }
    Ok(())
}

fn create_schema_v2(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
         ) STRICT;
         CREATE TABLE automations (
            id TEXT PRIMARY KEY,
            thread_id TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision >= 1),
            state TEXT NOT NULL,
            name TEXT NOT NULL,
            prompt TEXT NOT NULL,
            view_json TEXT NOT NULL,
            next_due_ms INTEGER,
            created_ms INTEGER NOT NULL,
            updated_ms INTEGER NOT NULL
         ) STRICT;
         CREATE INDEX automations_thread_due
           ON automations(thread_id, state, next_due_ms);
         CREATE TABLE runs (
            id TEXT PRIMARY KEY,
            thread_id TEXT NOT NULL,
            automation_id TEXT NOT NULL REFERENCES automations(id),
            revision INTEGER NOT NULL CHECK(revision >= 1),
            state TEXT NOT NULL,
            logical_occurrence_id TEXT NOT NULL UNIQUE,
            nominal_due_ms INTEGER NOT NULL,
            client_user_message_id TEXT NOT NULL,
            input_digest TEXT NOT NULL,
            remote_submission_id TEXT,
            view_json TEXT NOT NULL,
            updated_ms INTEGER NOT NULL
         ) STRICT;
         CREATE INDEX runs_thread_due ON runs(thread_id, nominal_due_ms DESC);
         CREATE INDEX runs_automation_due ON runs(automation_id, nominal_due_ms DESC);
         CREATE INDEX runs_client_id ON runs(client_user_message_id);
         CREATE INDEX runs_retention ON runs(state, updated_ms, id);
         CREATE TABLE request_keys (
            scope_hash TEXT PRIMARY KEY,
            response_json TEXT NOT NULL,
            expires_ms INTEGER NOT NULL
         ) STRICT;
         CREATE INDEX request_keys_expiry ON request_keys(expires_ms);
         CREATE TABLE task_grants (
            thread_id TEXT PRIMARY KEY,
            expires_ms INTEGER NOT NULL,
            granted_ms INTEGER NOT NULL,
            remaining_uses INTEGER NOT NULL DEFAULT 1 CHECK(remaining_uses >= 0)
         ) STRICT;
         CREATE INDEX task_grants_expiry ON task_grants(expires_ms);
         CREATE TABLE events (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            data_json TEXT NOT NULL,
            created_ms INTEGER NOT NULL
         ) STRICT;
         CREATE INDEX events_thread_created ON events(thread_id, created_ms, sequence);
         CREATE INDEX events_created ON events(created_ms, sequence);",
    )?;
    Ok(())
}

fn migrate_v1_to_v2(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE task_grants
           ADD COLUMN remaining_uses INTEGER NOT NULL DEFAULT 1 CHECK(remaining_uses >= 0);
         CREATE INDEX IF NOT EXISTS runs_retention ON runs(state, updated_ms, id);
         CREATE INDEX IF NOT EXISTS request_keys_expiry ON request_keys(expires_ms);
         CREATE INDEX IF NOT EXISTS task_grants_expiry ON task_grants(expires_ms);
         CREATE INDEX IF NOT EXISTS events_thread_created
           ON events(thread_id, created_ms, sequence);
         CREATE INDEX IF NOT EXISTS events_created ON events(created_ms, sequence);",
    )?;
    connection.execute(
        "UPDATE meta SET value=?1 WHERE key='schema_version'",
        [SCHEMA_VERSION.to_string()],
    )?;
    validate_schema_v2(connection)
}

fn validate_schema_v2(connection: &Connection) -> Result<()> {
    for table in [
        "meta",
        "automations",
        "runs",
        "request_keys",
        "task_grants",
        "events",
    ] {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
             )",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("schema version 2 is missing required table {table}");
        }
    }
    let has_remaining_uses: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pragma_table_info('task_grants')
             WHERE name='remaining_uses' AND type='INTEGER' AND \"notnull\"=1
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_remaining_uses {
        bail!("schema version 2 is missing task_grants.remaining_uses");
    }
    Ok(())
}

fn enforce_automation_quota(connection: &Connection, thread_id: &str) -> Result<()> {
    let per_task: i64 = connection.query_row(
        "SELECT COUNT(*) FROM automations WHERE thread_id=?1",
        [thread_id],
        |row| row.get(0),
    )?;
    if u64::try_from(per_task).context("negative automation count")? >= MAX_AUTOMATIONS_PER_TASK {
        bail!("quota exceeded: automations per task (limit {MAX_AUTOMATIONS_PER_TASK})");
    }
    if count(connection, "SELECT COUNT(*) FROM automations")? >= MAX_AUTOMATIONS_GLOBAL {
        bail!("quota exceeded: automations global (limit {MAX_AUTOMATIONS_GLOBAL})");
    }
    Ok(())
}

fn prune_and_enforce_run_quotas(
    connection: &Connection,
    thread_id: &str,
    now_ms: i64,
) -> Result<()> {
    let cutoff = now_ms.saturating_sub(TERMINAL_RUN_RETENTION_MS);
    connection.execute(
        &format!(
            "DELETE FROM runs WHERE id IN (
                 SELECT id FROM runs
                 WHERE state IN ({TERMINAL_RUN_STATES_SQL}) AND updated_ms < ?1
                 ORDER BY updated_ms ASC, id ASC
             )"
        ),
        [cutoff],
    )?;

    prune_terminal_runs_for_task(connection, thread_id, MAX_RUNS_PER_TASK)?;
    let per_task: i64 = connection.query_row(
        "SELECT COUNT(*) FROM runs WHERE thread_id=?1",
        [thread_id],
        |row| row.get(0),
    )?;
    if u64::try_from(per_task).context("negative run count")? >= MAX_RUNS_PER_TASK {
        bail!("quota exceeded: runs per task (limit {MAX_RUNS_PER_TASK})");
    }

    prune_terminal_runs_global(connection, MAX_RUNS_GLOBAL)?;
    if count(connection, "SELECT COUNT(*) FROM runs")? >= MAX_RUNS_GLOBAL {
        bail!("quota exceeded: runs global (limit {MAX_RUNS_GLOBAL})");
    }
    Ok(())
}

fn prune_terminal_runs_for_task(
    connection: &Connection,
    thread_id: &str,
    limit: u64,
) -> Result<()> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM runs WHERE thread_id=?1",
        [thread_id],
        |row| row.get(0),
    )?;
    let total = u64::try_from(total).context("negative run count")?;
    if total < limit {
        return Ok(());
    }
    let remove = total.saturating_sub(limit).saturating_add(1);
    connection.execute(
        &format!(
            "DELETE FROM runs WHERE id IN (
                 SELECT id FROM runs
                 WHERE thread_id=?1 AND state IN ({TERMINAL_RUN_STATES_SQL})
                 ORDER BY updated_ms ASC, id ASC LIMIT ?2
             )"
        ),
        params![thread_id, i64_from_u64(remove)?],
    )?;
    Ok(())
}

fn prune_terminal_runs_global(connection: &Connection, limit: u64) -> Result<()> {
    let total = count(connection, "SELECT COUNT(*) FROM runs")?;
    if total < limit {
        return Ok(());
    }
    let remove = total.saturating_sub(limit).saturating_add(1);
    connection.execute(
        &format!(
            "DELETE FROM runs WHERE id IN (
                 SELECT id FROM runs
                 WHERE state IN ({TERMINAL_RUN_STATES_SQL})
                 ORDER BY updated_ms ASC, id ASC LIMIT ?1
             )"
        ),
        [i64_from_u64(remove)?],
    )?;
    Ok(())
}

fn prune_events(connection: &Connection, thread_id: &str, now_ms: i64) -> Result<()> {
    let cutoff = now_ms.saturating_sub(EVENT_RETENTION_MS);
    connection.execute("DELETE FROM events WHERE created_ms < ?1", [cutoff])?;
    prune_events_for_task(connection, thread_id, MAX_EVENTS_PER_TASK)?;
    prune_events_global(connection, MAX_EVENTS_GLOBAL)?;
    Ok(())
}

fn prune_events_for_task(connection: &Connection, thread_id: &str, limit: u64) -> Result<()> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM events WHERE thread_id=?1",
        [thread_id],
        |row| row.get(0),
    )?;
    let total = u64::try_from(total).context("negative event count")?;
    if total < limit {
        return Ok(());
    }
    let remove = total.saturating_sub(limit).saturating_add(1);
    connection.execute(
        "DELETE FROM events WHERE sequence IN (
             SELECT sequence FROM events WHERE thread_id=?1
             ORDER BY created_ms ASC, sequence ASC LIMIT ?2
         )",
        params![thread_id, i64_from_u64(remove)?],
    )?;
    Ok(())
}

fn prune_events_global(connection: &Connection, limit: u64) -> Result<()> {
    let total = count(connection, "SELECT COUNT(*) FROM events")?;
    if total < limit {
        return Ok(());
    }
    let remove = total.saturating_sub(limit).saturating_add(1);
    connection.execute(
        "DELETE FROM events WHERE sequence IN (
             SELECT sequence FROM events ORDER BY created_ms ASC, sequence ASC LIMIT ?1
         )",
        [i64_from_u64(remove)?],
    )?;
    Ok(())
}

fn decode_automation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredAutomation> {
    let view_json: String = row.get(0)?;
    let prompt: String = row.get(1)?;
    Ok(StoredAutomation {
        view: from_json_column(&view_json)?,
        prompt,
    })
}

fn decode_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRun> {
    let view_json: String = row.get(0)?;
    Ok(StoredRun {
        view: from_json_column(&view_json)?,
        client_user_message_id: row.get(1)?,
        input_digest: row.get(2)?,
        remote_submission_id: row.get(3)?,
    })
}

fn from_json_column<T: for<'de> Deserialize<'de>>(json: &str) -> rusqlite::Result<T> {
    serde_json::from_str(json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn enum_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn i64_from_u64(value: u64) -> Result<i64> {
    i64::try_from(value).context("revision exceeds SQLite integer range")
}

fn parse_cursor(cursor: Option<&str>) -> Result<usize> {
    cursor.map_or(Ok(0), |value| {
        value.parse::<usize>().context("invalid pagination cursor")
    })
}

fn count(connection: &Connection, sql: &str) -> Result<u64> {
    let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    u64::try_from(value).context("negative SQLite count")
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn prepare_private_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("refusing insecure database parent {}", path.display());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o777 != 0o700 {
                    bail!(
                        "refusing to repermission database parent {}",
                        path.display()
                    );
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder
                    .create(path)
                    .with_context(|| format!("failed to create {}", path.display()))?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AuthorityState, AuthoritySummary, AutomationKind, AutomationTarget,
        NormalizedCatchUpPolicy, NormalizedSchedule, PromptSummary, TargetSource,
    };
    use tempfile::TempDir;

    fn private_tempdir() -> TempDir {
        let directory = TempDir::new().expect("tempdir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .expect("private tempdir mode");
        }
        directory
    }

    fn automation(thread_id: &str) -> StoredAutomation {
        let now = Utc::now();
        StoredAutomation {
            prompt: "Check the build".to_owned(),
            view: AutomationView {
                id: "automation-1".to_owned(),
                revision: 1,
                name: "Build check".to_owned(),
                prompt: PromptSummary {
                    sha256: "abc".to_owned(),
                    byte_length: 15,
                    redacted_preview: "Check…".to_owned(),
                },
                kind: AutomationKind::Heartbeat,
                target: AutomationTarget {
                    thread_id: thread_id.to_owned(),
                    source: TargetSource::OriginatingTask,
                },
                schedule: NormalizedSchedule::Once { at: now },
                catch_up: NormalizedCatchUpPolicy::default(),
                state: AutomationState::Active,
                next_nominal_due_at: Some(now),
                pending_run_count: 0,
                authority: AuthoritySummary {
                    fingerprint: "test".to_owned(),
                    state: AuthorityState::Matched,
                    auto_review: false,
                    reauthorization_required: false,
                },
                created_at: now,
                updated_at: now,
                blocker: None,
            },
        }
    }

    fn automation_with_id(thread_id: &str, id: usize) -> StoredAutomation {
        let mut record = automation(thread_id);
        record.view.id = format!("automation-{id}");
        record.view.name = format!("Build check {id}");
        record
    }

    #[tokio::test]
    async fn automation_is_task_scoped() {
        let directory = private_tempdir();
        let store = Store::open(directory.path().join("threadbeat.db"))
            .await
            .expect("open store");
        store
            .create_automation(automation("thread-1"))
            .await
            .expect("create automation");
        assert!(
            store
                .get_automation("thread-1".to_owned(), "automation-1".to_owned())
                .await
                .expect("get own automation")
                .is_some()
        );
        assert!(
            store
                .get_automation("thread-2".to_owned(), "automation-1".to_owned())
                .await
                .expect("get other automation")
                .is_none()
        );
    }

    #[tokio::test]
    async fn optimistic_revision_rejects_stale_update() {
        let directory = private_tempdir();
        let store = Store::open(directory.path().join("threadbeat.db"))
            .await
            .expect("open store");
        let mut record = automation("thread-1");
        store
            .create_automation(record.clone())
            .await
            .expect("create automation");
        record.view.revision = 2;
        assert!(
            store
                .replace_automation("thread-1".to_owned(), 1, record.clone())
                .await
                .expect("first update")
        );
        record.view.revision = 3;
        assert!(
            !store
                .replace_automation("thread-1".to_owned(), 1, record)
                .await
                .expect("stale update")
        );
    }

    #[tokio::test]
    async fn fresh_schema_uses_one_shot_task_authorization() {
        let directory = private_tempdir();
        let store = Store::open(directory.path().join("threadbeat.db"))
            .await
            .expect("open store");
        let now = Utc::now();
        store
            .authorize_task("thread-1".to_owned(), now + chrono::Duration::hours(1))
            .await
            .expect("authorize task");

        assert!(
            store
                .task_authorized("thread-1".to_owned(), now)
                .await
                .expect("read authorization")
        );
        assert!(
            store
                .task_authorized("thread-1".to_owned(), now)
                .await
                .expect("read authorization again")
        );
        assert!(
            store
                .consume_task_authorization("thread-1".to_owned(), now)
                .await
                .expect("consume authorization")
        );
        assert!(
            !store
                .consume_task_authorization("thread-1".to_owned(), now)
                .await
                .expect("reject second consume")
        );
        assert!(
            !store
                .task_authorized("thread-1".to_owned(), now)
                .await
                .expect("read consumed authorization")
        );
        assert_eq!(store.stats().await.expect("stats").schema_version, 2);
    }

    #[tokio::test]
    async fn idempotency_completion_is_compare_and_set() {
        let directory = private_tempdir();
        let store = Store::open(directory.path().join("threadbeat.db"))
            .await
            .expect("open store");
        let expires_at = Utc::now() + chrono::Duration::hours(1);
        let pending = serde_json::json!({"status": "pending", "owner": "one"});
        assert!(
            store
                .idempotency_reserve("scope".to_owned(), pending.clone(), expires_at)
                .await
                .expect("reserve")
                .is_none()
        );

        let first = serde_json::json!({"status": "succeeded", "result": 1});
        assert!(
            store
                .idempotency_complete(
                    "scope".to_owned(),
                    pending.clone(),
                    first.clone(),
                    expires_at,
                )
                .await
                .expect("first completion")
        );
        assert!(
            !store
                .idempotency_complete(
                    "scope".to_owned(),
                    pending,
                    serde_json::json!({"status": "succeeded", "result": 2}),
                    expires_at,
                )
                .await
                .expect("stale completion")
        );
        assert_eq!(
            store
                .idempotency_get("scope".to_owned())
                .await
                .expect("read completion"),
            Some(first)
        );
    }

    #[tokio::test]
    async fn migration_from_v1_preserves_grant_as_one_use() {
        let directory = private_tempdir();
        let path = directory.path().join("threadbeat.db");
        let now = Utc::now();
        {
            let connection = Connection::open(&path).expect("open v1 database");
            create_schema_v2(&connection).expect("create base schema");
            connection
                .execute_batch(
                    "DROP INDEX task_grants_expiry;
                     DROP TABLE task_grants;
                     CREATE TABLE task_grants (
                         thread_id TEXT PRIMARY KEY,
                         expires_ms INTEGER NOT NULL,
                         granted_ms INTEGER NOT NULL
                     ) STRICT;",
                )
                .expect("restore v1 task grants");
            connection
                .execute(
                    "INSERT INTO meta(key, value) VALUES ('schema_version', '1')",
                    [],
                )
                .expect("set v1 version");
            connection
                .execute(
                    "INSERT INTO task_grants(thread_id, expires_ms, granted_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        "thread-1",
                        (now + chrono::Duration::hours(1)).timestamp_millis(),
                        now.timestamp_millis()
                    ],
                )
                .expect("insert v1 grant");
        }

        let store = Store::open(&path).await.expect("migrate v1 store");
        assert!(
            store
                .consume_task_authorization("thread-1".to_owned(), now)
                .await
                .expect("consume migrated grant")
        );
        assert!(
            !store
                .consume_task_authorization("thread-1".to_owned(), now)
                .await
                .expect("migrated grant is one use")
        );
    }

    #[tokio::test]
    async fn future_schema_version_is_rejected_without_overwrite() {
        let directory = private_tempdir();
        let path = directory.path().join("threadbeat.db");
        {
            let connection = Connection::open(&path).expect("open future database");
            connection
                .execute_batch(
                    "CREATE TABLE meta (
                         key TEXT PRIMARY KEY,
                         value TEXT NOT NULL
                     ) STRICT;
                     INSERT INTO meta(key, value) VALUES ('schema_version', '999');",
                )
                .expect("create future schema marker");
        }

        let error = Store::open(&path)
            .await
            .err()
            .expect("future schema must fail closed");
        assert!(error.to_string().contains("newer than supported"));
        let connection = Connection::open(&path).expect("reopen future database");
        let version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("read untouched version");
        assert_eq!(version, "999");
        let has_automations: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type='table' AND name='automations'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("inspect untouched schema");
        assert!(!has_automations);
    }

    #[tokio::test]
    async fn automation_quota_is_deterministic() {
        let directory = private_tempdir();
        let store = Store::open(directory.path().join("threadbeat.db"))
            .await
            .expect("open store");
        for id in 0..MAX_AUTOMATIONS_PER_TASK {
            store
                .create_automation(automation_with_id(
                    "thread-1",
                    usize::try_from(id).expect("small id"),
                ))
                .await
                .expect("create within quota");
        }
        let error = store
            .create_automation(automation_with_id(
                "thread-1",
                usize::try_from(MAX_AUTOMATIONS_PER_TASK).expect("small id"),
            ))
            .await
            .expect_err("quota must reject another automation");
        assert_eq!(
            error.to_string(),
            format!("quota exceeded: automations per task (limit {MAX_AUTOMATIONS_PER_TASK})")
        );
    }

    #[test]
    fn oldest_terminal_runs_and_events_are_pruned_for_capacity() {
        let connection = Connection::open_in_memory().expect("open memory database");
        create_schema_v2(&connection).expect("create schema");
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION.to_string()],
            )
            .expect("set schema version");
        connection
            .execute(
                "INSERT INTO automations
                 (id, thread_id, revision, state, name, prompt, view_json,
                  next_due_ms, created_ms, updated_ms)
                 VALUES ('automation-1', 'thread-1', 1, '\"active\"', 'name',
                         'prompt', '{}', NULL, 0, 0)",
                [],
            )
            .expect("insert automation");
        for sequence in 0..3 {
            connection
                .execute(
                    "INSERT INTO runs
                     (id, thread_id, automation_id, revision, state,
                      logical_occurrence_id, nominal_due_ms, client_user_message_id,
                      input_digest, remote_submission_id, view_json, updated_ms)
                     VALUES (?1, 'thread-1', 'automation-1', 1, '\"succeeded\"',
                             ?2, ?3, ?4, 'digest', NULL, '{}', ?3)",
                    params![
                        format!("run-{sequence}"),
                        format!("occurrence-{sequence}"),
                        sequence,
                        format!("message-{sequence}")
                    ],
                )
                .expect("insert terminal run");
            connection
                .execute(
                    "INSERT INTO events(thread_id, kind, data_json, created_ms)
                     VALUES ('thread-1', 'test', '{}', ?1)",
                    [sequence],
                )
                .expect("insert event");
        }

        prune_terminal_runs_for_task(&connection, "thread-1", 2).expect("prune runs");
        prune_events_for_task(&connection, "thread-1", 2).expect("prune events");
        assert_eq!(
            count(&connection, "SELECT COUNT(*) FROM runs").expect("count runs"),
            1
        );
        assert_eq!(
            count(&connection, "SELECT COUNT(*) FROM events").expect("count events"),
            1
        );
        let retained_run: String = connection
            .query_row("SELECT id FROM runs", [], |row| row.get(0))
            .expect("retained run");
        assert_eq!(retained_run, "run-2");
    }
}
