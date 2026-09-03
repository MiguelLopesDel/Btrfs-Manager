use crate::policy::validate_policy;
use crate::state::StateStore;
use crate::validate::validate_relative_btrfs_path;
use crate::{
    CommandRunner, Helper, HelperError, HelperResponse, RetentionRunFailure, RetentionRunOutcome,
};
use btrfs_manager_core::models::{Snapshot, SnapshotOrigin, SnapshotPolicy, SnapshotState};
use btrfs_manager_core::naming::snapshot_name_from_path;
use btrfs_manager_core::retention::{RetentionPolicy, retention_keep_set};
use btrfs_manager_core::{PolicyRunLog, PolicyRunStatus, RetentionPreview};
use chrono::{Local, Utc};
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn preview_retention_impl(
        &self,
        policy_id: Uuid,
    ) -> Result<HelperResponse, HelperError> {
        let preview = self.retention_preview(policy_id)?;
        Ok(HelperResponse {
            ok: true,
            message: format!("{} snapshot(s) would be deleted", preview.delete.len()),
            data: Some(serde_json::to_value(preview)?),
        })
    }

    pub(crate) fn preview_retention_for_policy_impl(
        &self,
        policy: SnapshotPolicy,
    ) -> Result<HelperResponse, HelperError> {
        validate_policy(&policy)?;
        let snapshots = self
            .state_store_for_mountpoint(&policy.mountpoint)?
            .list_managed_snapshots_for_policy(policy.id)?;
        let preview = retention_preview_for_policy(&policy, &snapshots);
        Ok(HelperResponse {
            ok: true,
            message: format!("{} snapshot(s) would be deleted", preview.delete.len()),
            data: Some(serde_json::to_value(preview)?),
        })
    }

    pub(crate) fn run_retention_policy_impl(
        &self,
        policy_id: Uuid,
    ) -> Result<HelperResponse, HelperError> {
        let log = self.run_retention_policy(policy_id)?;
        Ok(HelperResponse {
            ok: matches!(log.status, PolicyRunStatus::Success),
            message: match log.status {
                PolicyRunStatus::Success => "snapshot policy executed".into(),
                PolicyRunStatus::Failed => log
                    .error
                    .clone()
                    .unwrap_or_else(|| "snapshot policy failed".into()),
            },
            data: Some(serde_json::to_value(log)?),
        })
    }

    pub(crate) fn list_policy_run_logs_impl(
        &self,
        policy_id: Uuid,
    ) -> Result<HelperResponse, HelperError> {
        let logs = self.default_state_store()?.list_policy_logs(policy_id)?;
        Ok(HelperResponse {
            ok: true,
            message: format!("found {} policy run log(s)", logs.len()),
            data: Some(serde_json::to_value(logs)?),
        })
    }

    pub(crate) fn retention_preview(
        &self,
        policy_id: Uuid,
    ) -> Result<RetentionPreview, HelperError> {
        let default_store = self.default_state_store()?;
        let policy = default_store
            .get_policy(policy_id)?
            .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
        let store = self.state_store_for_mountpoint(&policy.mountpoint)?;
        let snapshots = store.list_managed_snapshots_for_policy(policy_id)?;
        Ok(retention_preview_for_policy(&policy, &snapshots))
    }

    pub(crate) fn run_retention_policy(
        &self,
        policy_id: Uuid,
    ) -> Result<PolicyRunLog, HelperError> {
        let started_at = Utc::now();
        let log_id = Uuid::new_v4();
        let result = self.run_retention_policy_inner(policy_id);
        let finished_at = Utc::now();
        let log = match result {
            Ok(outcome) => PolicyRunLog {
                id: log_id,
                policy_id,
                started_at,
                finished_at,
                status: PolicyRunStatus::Success,
                created_snapshot: outcome.created_snapshot,
                deleted_snapshots: outcome.deleted_snapshots,
                error: None,
            },
            Err(err) => PolicyRunLog {
                id: log_id,
                policy_id,
                started_at,
                finished_at,
                status: PolicyRunStatus::Failed,
                created_snapshot: err.created_snapshot,
                deleted_snapshots: err.deleted_snapshots,
                error: Some(err.error.to_string()),
            },
        };
        self.default_state_store()?.insert_policy_run_log(&log)?;
        Ok(log)
    }

    fn run_retention_policy_inner(
        &self,
        policy_id: Uuid,
    ) -> Result<RetentionRunOutcome, RetentionRunFailure> {
        let mut outcome = RetentionRunOutcome::default();
        match self.execute_retention_run(policy_id, &mut outcome) {
            Ok(()) => Ok(outcome),
            Err(error) => Err(RetentionRunFailure {
                created_snapshot: outcome.created_snapshot,
                deleted_snapshots: outcome.deleted_snapshots,
                error,
            }),
        }
    }

    fn execute_retention_run(
        &self,
        policy_id: Uuid,
        outcome: &mut RetentionRunOutcome,
    ) -> Result<(), HelperError> {
        let policy = self.load_enabled_policy(policy_id)?;
        let top = self.ensure_top_level_mount(&policy.mountpoint)?;
        let store = Self::state_store_at_top_level(&top)?;
        let existing = store.list_managed_snapshots_for_policy(policy_id)?;
        let keep = retention_keep_set(&existing, &retention_policy_from_snapshot_policy(&policy));
        let delete_candidates: Vec<_> = existing
            .into_iter()
            .filter(|snapshot| {
                !keep.contains(&snapshot.id)
                    && snapshot.is_managed()
                    && snapshot.state != SnapshotState::RollbackAnchor
            })
            .collect();

        self.create_scheduled_snapshot(&top, &store, &policy, policy_id, outcome)?;
        self.apply_retention_deletes(&top, &store, delete_candidates, outcome)
    }

    fn load_enabled_policy(&self, policy_id: Uuid) -> Result<SnapshotPolicy, HelperError> {
        let policy = self
            .default_state_store()?
            .get_policy(policy_id)?
            .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
        if !policy.enabled {
            return Err(HelperError::InvalidPolicy(format!(
                "policy {policy_id} is disabled"
            )));
        }
        validate_policy(&policy)?;
        Ok(policy)
    }

    /// Create the scheduled snapshot and record it in SQLite. If recording
    /// fails, the freshly created subvolume is removed again so filesystem and
    /// state never diverge.
    fn create_scheduled_snapshot(
        &self,
        top: &Path,
        store: &StateStore,
        policy: &SnapshotPolicy,
        policy_id: Uuid,
        outcome: &mut RetentionRunOutcome,
    ) -> Result<(), HelperError> {
        // Create snapshot container subvolume if needed.
        let container = top.join(&policy.snapshot_root);
        if !container.exists() {
            self.runner.run(
                "btrfs",
                &[
                    "subvolume".into(),
                    "create".into(),
                    container.display().to_string(),
                ],
            )?;
        }
        let snap_dir_abs = top.join(policy_snapshot_dir(policy));
        std::fs::create_dir_all(&snap_dir_abs)?;

        let dest_name = snapshot_name_from_path(&policy.source_path, Local::now());
        let dest_abs = snap_dir_abs.join(&dest_name);
        let source_abs = top.join(&policy.source_path);

        self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "snapshot".into(),
                "-r".into(),
                source_abs.display().to_string(),
                dest_abs.display().to_string(),
            ],
        )?;

        // Store relative path in SQLite.
        let rel_path = policy_snapshot_dir(policy).join(&dest_name);
        outcome.created_snapshot = Some(rel_path.clone());
        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            source_subvolume: policy.subvolume_id.clone(),
            path: rel_path.clone(),
            created_at: Utc::now(),
            tags: vec!["scheduled".into()],
            origin: SnapshotOrigin::Managed,
            state: SnapshotState::ReadOnly,
        };
        if let Err(insert_error) = store.insert_managed_snapshot(Some(policy_id), &snapshot) {
            match self.delete_retention_snapshot_subvolume(top, &rel_path) {
                Ok(_) => {
                    outcome.created_snapshot = None;
                    return Err(HelperError::InvalidPolicy(format!(
                        "failed to record scheduled snapshot {}; removed the created snapshot during recovery: {insert_error}",
                        rel_path.display()
                    )));
                }
                Err(cleanup_error) => {
                    return Err(HelperError::InvalidPolicy(format!(
                        "failed to record scheduled snapshot {}; cleanup also failed: {insert_error}; cleanup error: {cleanup_error}",
                        rel_path.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Retention deletes are based on the same pre-run snapshot set used
    /// by preview, so a run does not delete more than preview showed.
    fn apply_retention_deletes(
        &self,
        top: &Path,
        store: &StateStore,
        delete_candidates: Vec<Snapshot>,
        outcome: &mut RetentionRunOutcome,
    ) -> Result<(), HelperError> {
        for old in delete_candidates {
            validate_relative_btrfs_path(&old.path, "managed snapshot path from state")?;
            let deleted = match self.delete_retention_snapshot_subvolume(top, &old.path) {
                Ok(deleted) => deleted,
                Err(delete_error) if !top.join(&old.path).exists() => {
                    tracing::warn!(
                        path = %old.path.display(),
                        error = %delete_error,
                        "retention delete failed but snapshot path is now missing; cleaning stale state"
                    );
                    false
                }
                Err(delete_error) => return Err(delete_error),
            };
            if deleted {
                outcome.deleted_snapshots.push(old.path.clone());
            } else {
                tracing::warn!(
                    path = %old.path.display(),
                    "retention found stale snapshot state without a matching subvolume"
                );
            }
            store.delete_managed_snapshot(old.id)?;
        }
        Ok(())
    }

    fn delete_retention_snapshot_subvolume(
        &self,
        top: &Path,
        path: &Path,
    ) -> Result<bool, HelperError> {
        validate_relative_btrfs_path(path, "managed snapshot path from state")?;
        let abs_path = top.join(path);
        if !abs_path.exists() {
            return Ok(false);
        }
        self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "delete".into(),
                abs_path.display().to_string(),
            ],
        )?;
        Ok(true)
    }
}

fn retention_policy_from_snapshot_policy(policy: &SnapshotPolicy) -> RetentionPolicy {
    RetentionPolicy {
        hourly: policy.keep_hourly,
        daily: policy.keep_daily,
        weekly: policy.keep_weekly,
        monthly: policy.keep_monthly,
    }
}

pub(crate) fn retention_preview_for_policy(
    policy: &SnapshotPolicy,
    snapshots: &[Snapshot],
) -> RetentionPreview {
    let keep_ids = retention_keep_set(snapshots, &retention_policy_from_snapshot_policy(policy));
    let mut keep = Vec::new();
    let mut delete = Vec::new();
    for snapshot in snapshots {
        if keep_ids.contains(&snapshot.id)
            || !snapshot.is_managed()
            || snapshot.state == SnapshotState::RollbackAnchor
        {
            keep.push(snapshot.clone());
        } else {
            delete.push(snapshot.clone());
        }
    }
    RetentionPreview {
        policy_id: policy.id,
        next_snapshot_path: policy_snapshot_dir(policy)
            .join(snapshot_name_from_path(&policy.source_path, Local::now())),
        delete,
        keep,
    }
}

// Returns a path relative to the Btrfs volume root (used for SQLite storage
// and preview display). Callers that need an absolute path join with the
// top-level mount point.
pub(crate) fn policy_snapshot_dir(policy: &SnapshotPolicy) -> PathBuf {
    policy.snapshot_root.join(policy.id.to_string())
}
