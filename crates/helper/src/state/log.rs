use super::connection::StateStore;
use super::convert::{make_sql_conv_error, parse_datetime_for_sql, parse_uuid_for_sql};
use crate::HelperError;
use btrfs_manager_core::{PolicyRunLog, PolicyRunStatus};
use rusqlite::params;
use std::path::PathBuf;
use uuid::Uuid;

impl StateStore {
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
