use super::connection::StateStore;
use super::convert::parse_datetime_for_sql;
use super::convert::parse_uuid_for_sql;
use crate::HelperError;
use btrfs_manager_core::models::{Snapshot, SnapshotOrigin, SnapshotState};
use rusqlite::{OptionalExtension, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl StateStore {
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
