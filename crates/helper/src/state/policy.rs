use super::connection::StateStore;
use super::convert::parse_uuid_for_sql;
use crate::HelperError;
use btrfs_manager_core::models::SnapshotPolicy;
use rusqlite::{OptionalExtension, params};
use std::path::PathBuf;
use uuid::Uuid;

impl StateStore {
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

    pub(crate) fn delete_policy(&self, policy_id: Uuid) -> Result<(), HelperError> {
        self.connection.execute(
            "DELETE FROM snapshot_policies WHERE id = ?1",
            params![policy_id.to_string()],
        )?;
        Ok(())
    }
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
