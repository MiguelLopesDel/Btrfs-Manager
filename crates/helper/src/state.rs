use crate::HelperError;
use btrfs_manager_core::models::{
    BootIntegration, PolicyRunLog, PolicyRunStatus, Snapshot, SnapshotOrigin, SnapshotPolicy,
    SnapshotState,
};
use btrfs_manager_core::rollback::{RollbackPlan, RollbackStatus};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) struct StateStore {
    connection: Connection,
}

impl StateStore {
    pub(crate) fn open_at(path: PathBuf) -> Result<Self, HelperError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn open() -> Result<Self, HelperError> {
        Self::open_at(state_db_path())
    }

    fn migrate(&self) -> Result<(), HelperError> {
        self.connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS managed_snapshots (
                id TEXT PRIMARY KEY NOT NULL,
                policy_id TEXT,
                source_subvolume_id INTEGER NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                tags_json TEXT NOT NULL DEFAULT '[]',
                origin_tool TEXT,
                state TEXT NOT NULL CHECK (state IN ('readonly', 'unlocked', 'dirty_unlocked', 'rollback_anchor'))
            );
            CREATE TABLE IF NOT EXISTS snapshot_policies (
                id TEXT PRIMARY KEY NOT NULL,
                filesystem_id TEXT,
                subvolume_id INTEGER NOT NULL,
                source_path TEXT NOT NULL,
                mountpoint TEXT NOT NULL,
                snapshot_root TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                schedule TEXT NOT NULL CHECK (schedule IN ('hourly', 'daily', 'weekly', 'monthly')),
                keep_hourly INTEGER NOT NULL DEFAULT 24,
                keep_daily INTEGER NOT NULL DEFAULT 7,
                keep_weekly INTEGER NOT NULL DEFAULT 4,
                keep_monthly INTEGER NOT NULL DEFAULT 6,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS policy_run_logs (
                id TEXT PRIMARY KEY NOT NULL,
                policy_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('success', 'failed')),
                created_snapshot TEXT,
                deleted_snapshots_json TEXT NOT NULL DEFAULT '[]',
                error TEXT
            );
            CREATE TABLE IF NOT EXISTS rollback_plans (
                id TEXT PRIMARY KEY NOT NULL,
                mountpoint TEXT NOT NULL,
                source_snapshot_path TEXT NOT NULL,
                replaced_subvol_path TEXT NOT NULL,
                return_snapshot_path TEXT NOT NULL,
                boot_integration TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('awaiting_reboot', 'activated', 'reverted', 'failed')),
                created_at TEXT NOT NULL,
                created_boot_id TEXT,
                description TEXT
            );
            "#,
        )?;
        add_column_if_missing(&self.connection, "managed_snapshots", "policy_id", "TEXT")?;
        for (column, definition) in [
            ("filesystem_id", "TEXT"),
            ("source_path", "TEXT NOT NULL DEFAULT ''"),
            ("mountpoint", "TEXT NOT NULL DEFAULT '/'"),
            ("snapshot_root", "TEXT NOT NULL DEFAULT '.snapshots'"),
            ("created_at", "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"),
            ("updated_at", "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"),
        ] {
            add_column_if_missing(&self.connection, "snapshot_policies", column, definition)?;
        }
        add_column_if_missing(
            &self.connection,
            "rollback_plans",
            "created_boot_id",
            "TEXT",
        )?;
        add_column_if_missing(&self.connection, "rollback_plans", "description", "TEXT")?;
        Ok(())
    }

    pub(crate) fn list_policies(&self) -> Result<Vec<SnapshotPolicy>, HelperError> {
        let mut statement = self.connection.prepare(
            "SELECT id, filesystem_id, subvolume_id, source_path, mountpoint, snapshot_root, schedule, keep_hourly, keep_daily, keep_weekly, keep_monthly, enabled FROM snapshot_policies ORDER BY source_path",
        )?;
        let policies = statement
            .query_map([], policy_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(policies)
    }

    pub(crate) fn get_policy(&self, id: Uuid) -> Result<Option<SnapshotPolicy>, HelperError> {
        let result = self
            .connection
            .query_row(
                "SELECT id, filesystem_id, subvolume_id, source_path, mountpoint, snapshot_root, schedule, keep_hourly, keep_daily, keep_weekly, keep_monthly, enabled FROM snapshot_policies WHERE id = ?1",
                params![id.to_string()],
                policy_from_row,
            )
            .optional()?;
        Ok(result)
    }

    pub(crate) fn upsert_policy(&self, policy: &SnapshotPolicy) -> Result<(), HelperError> {
        self.connection.execute(
            "INSERT OR REPLACE INTO snapshot_policies (id, filesystem_id, subvolume_id, source_path, mountpoint, snapshot_root, schedule, keep_hourly, keep_daily, keep_weekly, keep_monthly, enabled, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, CURRENT_TIMESTAMP)",
            params![
                policy.id.to_string(),
                policy.filesystem_id.as_ref().map(|id| id.0.to_string()),
                policy.subvolume_id.0 as i64,
                policy.source_path.display().to_string(),
                policy.mountpoint.display().to_string(),
                policy.snapshot_root.display().to_string(),
                policy_schedule_to_db(&policy.schedule),
                policy.keep_hourly as i64,
                policy.keep_daily as i64,
                policy.keep_weekly as i64,
                policy.keep_monthly as i64,
                policy.enabled as i64,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn insert_managed_snapshot(
        &self,
        policy_id: Option<Uuid>,
        snapshot: &Snapshot,
    ) -> Result<(), HelperError> {
        let tags = serde_json::to_string(&snapshot.tags)?;
        self.connection.execute(
            "INSERT OR REPLACE INTO managed_snapshots (id, policy_id, source_subvolume_id, path, created_at, tags_json, origin_tool, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
            params![
                snapshot.id.to_string(),
                policy_id.map(|id| id.to_string()),
                snapshot.source_subvolume.0 as i64,
                snapshot.path.display().to_string(),
                snapshot.created_at.to_rfc3339(),
                tags,
                snapshot_state_to_db(&snapshot.state),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn list_all_managed_snapshots(&self) -> Result<Vec<Snapshot>, HelperError> {
        let mut stmt = self.connection.prepare(
            "SELECT id, source_subvolume_id, path, created_at, tags_json, origin_tool, state FROM managed_snapshots ORDER BY created_at DESC",
        )?;
        let snapshots = stmt
            .query_map([], snapshot_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(snapshots)
    }

    pub(crate) fn find_managed_snapshot_id_by_path(
        &self,
        path: &Path,
    ) -> Result<Uuid, HelperError> {
        let result: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM managed_snapshots WHERE path = ?1",
                params![path.display().to_string()],
                |row| row.get(0),
            )
            .optional()?;
        result
            .ok_or_else(|| {
                HelperError::InvalidPolicy(format!(
                    "no managed snapshot at path {}",
                    path.display()
                ))
            })
            .and_then(|id| {
                id.parse::<Uuid>()
                    .map_err(|e| HelperError::InvalidPolicy(format!("invalid uuid in db: {e}")))
            })
    }

    pub(crate) fn delete_managed_snapshot(&self, id: Uuid) -> Result<(), HelperError> {
        self.connection.execute(
            "DELETE FROM managed_snapshots WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn update_snapshot_state(
        &self,
        id: Uuid,
        state: &SnapshotState,
    ) -> Result<(), HelperError> {
        self.connection.execute(
            "UPDATE managed_snapshots SET state = ?1 WHERE id = ?2",
            params![snapshot_state_to_db(state), id.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn insert_rollback_plan(&self, plan: &RollbackPlan) -> Result<(), HelperError> {
        self.connection.execute(
            "INSERT INTO rollback_plans (id, mountpoint, source_snapshot_path, replaced_subvol_path, return_snapshot_path, boot_integration, status, created_at, created_boot_id, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'awaiting_reboot', ?7, ?8, ?9)",
            params![
                plan.id.to_string(),
                plan.mountpoint.display().to_string(),
                plan.source_snapshot_path.display().to_string(),
                plan.replaced_subvol_path.display().to_string(),
                plan.return_snapshot_path.display().to_string(),
                boot_integration_to_db(&plan.boot_integration),
                plan.created_at.to_rfc3339(),
                plan.created_boot_id,
                plan.description,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn get_pending_rollback(&self) -> Result<Option<RollbackPlan>, HelperError> {
        type Row = (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        );
        let result: Option<Row> = self
            .connection
            .query_row(
                "SELECT id, mountpoint, source_snapshot_path, replaced_subvol_path, return_snapshot_path, boot_integration, created_at, created_boot_id, description FROM rollback_plans WHERE status = 'awaiting_reboot' ORDER BY created_at DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
            )
            .optional()?;
        match result {
            None => Ok(None),
            Some((
                id,
                mountpoint,
                src,
                replaced,
                ret,
                boot,
                created_at,
                created_boot_id,
                description,
            )) => Ok(Some(RollbackPlan {
                id: id
                    .parse::<Uuid>()
                    .map_err(|e| HelperError::InvalidPolicy(format!("invalid plan uuid: {e}")))?,
                mountpoint: PathBuf::from(mountpoint),
                source_snapshot_path: PathBuf::from(src),
                replaced_subvol_path: PathBuf::from(replaced),
                return_snapshot_path: PathBuf::from(ret),
                boot_integration: boot_integration_from_db(&boot),
                status: RollbackStatus::AwaitingReboot,
                created_at: created_at.parse::<DateTime<Utc>>().map_err(|e| {
                    HelperError::InvalidPolicy(format!("invalid rollback created_at: {e}"))
                })?,
                created_boot_id,
                description,
            })),
        }
    }

    pub(crate) fn update_rollback_plan_status(
        &self,
        plan_id: Uuid,
        status: &str,
    ) -> Result<(), HelperError> {
        self.connection.execute(
            "UPDATE rollback_plans SET status = ?1 WHERE id = ?2",
            params![status, plan_id.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn list_managed_snapshots_for_policy(
        &self,
        policy_id: Uuid,
    ) -> Result<Vec<Snapshot>, HelperError> {
        let mut statement = self.connection.prepare(
            "SELECT id, source_subvolume_id, path, created_at, tags_json, origin_tool, state FROM managed_snapshots WHERE policy_id = ?1 ORDER BY created_at DESC",
        )?;
        let snapshots = statement
            .query_map(params![policy_id.to_string()], snapshot_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(snapshots)
    }

    pub(crate) fn list_policy_logs(
        &self,
        policy_id: Uuid,
    ) -> Result<Vec<PolicyRunLog>, HelperError> {
        let mut statement = self.connection.prepare(
            "SELECT id, policy_id, started_at, finished_at, status, created_snapshot, deleted_snapshots_json, error FROM policy_run_logs WHERE policy_id = ?1 ORDER BY started_at DESC LIMIT 50",
        )?;
        let logs = statement
            .query_map(params![policy_id.to_string()], policy_log_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(logs)
    }

    pub(crate) fn insert_policy_run_log(&self, log: &PolicyRunLog) -> Result<(), HelperError> {
        let deleted = serde_json::to_string(&log.deleted_snapshots)?;
        self.connection.execute(
            "INSERT INTO policy_run_logs (id, policy_id, started_at, finished_at, status, created_snapshot, deleted_snapshots_json, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                log.id.to_string(),
                log.policy_id.to_string(),
                log.started_at.to_rfc3339(),
                log.finished_at.to_rfc3339(),
                policy_run_status_to_db(&log.status),
                log.created_snapshot.as_ref().map(|p| p.display().to_string()),
                deleted,
                log.error,
            ],
        )?;
        Ok(())
    }
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), HelperError> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            params![table, column],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

fn policy_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotPolicy> {
    use btrfs_manager_core::models::{FilesystemId, SubvolumeId};
    let id: String = row.get(0)?;
    let filesystem_id: Option<String> = row.get(1)?;
    let subvolume_id: i64 = row.get(2)?;
    let source_path: String = row.get(3)?;
    let mountpoint: String = row.get(4)?;
    let snapshot_root: String = row.get(5)?;
    let schedule: String = row.get(6)?;
    Ok(SnapshotPolicy {
        id: parse_uuid_for_sql(id, 0)?,
        filesystem_id: filesystem_id
            .and_then(|s| s.parse::<uuid::Uuid>().ok())
            .map(FilesystemId),
        subvolume_id: SubvolumeId(subvolume_id as u64),
        source_path: PathBuf::from(source_path),
        mountpoint: PathBuf::from(mountpoint),
        snapshot_root: PathBuf::from(snapshot_root),
        schedule: policy_schedule_from_db(&schedule),
        keep_hourly: row.get::<_, i64>(7)? as usize,
        keep_daily: row.get::<_, i64>(8)? as usize,
        keep_weekly: row.get::<_, i64>(9)? as usize,
        keep_monthly: row.get::<_, i64>(10)? as usize,
        enabled: row.get::<_, i64>(11)? != 0,
    })
}

fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Snapshot> {
    use btrfs_manager_core::models::SubvolumeId;
    let id: String = row.get(0)?;
    let created_at: String = row.get(3)?;
    let tags_json: String = row.get(4)?;
    let origin_tool: Option<String> = row.get(5)?;
    let state: String = row.get(6)?;
    Ok(Snapshot {
        id: parse_uuid_for_sql(id, 0)?,
        source_subvolume: SubvolumeId(row.get::<_, i64>(1)? as u64),
        path: PathBuf::from(row.get::<_, String>(2)?),
        created_at: parse_datetime_for_sql(created_at, 3)?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        origin: origin_tool
            .map(|tool| SnapshotOrigin::External { tool: Some(tool) })
            .unwrap_or(SnapshotOrigin::Managed),
        state: snapshot_state_from_db(&state).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            )
        })?,
    })
}

fn policy_log_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PolicyRunLog> {
    let id: String = row.get(0)?;
    let policy_id: String = row.get(1)?;
    let started_at: String = row.get(2)?;
    let finished_at: String = row.get(3)?;
    let status: String = row.get(4)?;
    let created_snapshot: Option<String> = row.get(5)?;
    let deleted_json: String = row.get(6)?;
    Ok(PolicyRunLog {
        id: parse_uuid_for_sql(id, 0)?,
        policy_id: parse_uuid_for_sql(policy_id, 1)?,
        started_at: parse_datetime_for_sql(started_at, 2)?,
        finished_at: parse_datetime_for_sql(finished_at, 3)?,
        status: policy_run_status_from_db(&status).map_err(|e| make_sql_conv_error(4, e))?,
        created_snapshot: created_snapshot.map(PathBuf::from),
        deleted_snapshots: serde_json::from_str(&deleted_json).unwrap_or_default(),
        error: row.get(7)?,
    })
}

fn make_sql_conv_error(index: usize, err: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
    )
}

fn parse_uuid_for_sql(value: String, index: usize) -> rusqlite::Result<Uuid> {
    value
        .parse::<Uuid>()
        .map_err(|e| make_sql_conv_error(index, e.to_string()))
}

fn parse_datetime_for_sql(value: String, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    value
        .parse::<DateTime<Utc>>()
        .map_err(|e| make_sql_conv_error(index, e.to_string()))
}

fn snapshot_state_to_db(state: &SnapshotState) -> &'static str {
    match state {
        SnapshotState::ReadOnly => "readonly",
        SnapshotState::Unlocked => "unlocked",
        SnapshotState::DirtyUnlocked => "dirty_unlocked",
        SnapshotState::RollbackAnchor => "rollback_anchor",
    }
}

fn snapshot_state_from_db(value: &str) -> Result<SnapshotState, String> {
    match value {
        "readonly" => Ok(SnapshotState::ReadOnly),
        "unlocked" => Ok(SnapshotState::Unlocked),
        "dirty_unlocked" => Ok(SnapshotState::DirtyUnlocked),
        "rollback_anchor" => Ok(SnapshotState::RollbackAnchor),
        _ => Err(format!("unknown snapshot state: {value}")),
    }
}

fn policy_run_status_to_db(status: &PolicyRunStatus) -> &'static str {
    match status {
        PolicyRunStatus::Success => "success",
        PolicyRunStatus::Failed => "failed",
    }
}

fn policy_run_status_from_db(value: &str) -> Result<PolicyRunStatus, String> {
    match value {
        "success" => Ok(PolicyRunStatus::Success),
        "failed" => Ok(PolicyRunStatus::Failed),
        _ => Err(format!("unknown policy run status: {value}")),
    }
}

pub(crate) fn boot_integration_to_db(bi: &BootIntegration) -> &'static str {
    match bi {
        BootIntegration::GrubBtrfs => "grub_btrfs",
        BootIntegration::RefindBtrfs => "refind_btrfs",
        BootIntegration::Conservative => "conservative",
    }
}

pub(crate) fn boot_integration_from_db(value: &str) -> BootIntegration {
    match value {
        "grub_btrfs" => BootIntegration::GrubBtrfs,
        "refind_btrfs" => BootIntegration::RefindBtrfs,
        _ => BootIntegration::Conservative,
    }
}

fn policy_schedule_to_db(schedule: &btrfs_manager_core::models::PolicySchedule) -> &'static str {
    use btrfs_manager_core::models::PolicySchedule;
    match schedule {
        PolicySchedule::Hourly => "hourly",
        PolicySchedule::Daily => "daily",
        PolicySchedule::Weekly => "weekly",
        PolicySchedule::Monthly => "monthly",
    }
}

fn policy_schedule_from_db(value: &str) -> btrfs_manager_core::models::PolicySchedule {
    use btrfs_manager_core::models::PolicySchedule;
    match value {
        "hourly" => PolicySchedule::Hourly,
        "daily" => PolicySchedule::Daily,
        "weekly" => PolicySchedule::Weekly,
        _ => PolicySchedule::Monthly,
    }
}

#[cfg(test)]
pub(crate) fn state_db_path() -> PathBuf {
    std::env::var_os("BTRFS_MANAGER_STATE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/btrfs-manager/state.db"))
}
