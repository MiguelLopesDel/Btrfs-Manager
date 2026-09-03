use crate::boot::{current_boot_id, detect_boot_integration};
use crate::state::StateStore;
use crate::subvolume::mounted_subvolume_from_options;
use crate::validate::{validate_path, validate_relative_btrfs_path};
use crate::{CommandRunner, Helper, HelperError, HelperResponse};
use btrfs_manager_core::models::{
    BootIntegration, Snapshot, SnapshotOrigin, SnapshotState, SubvolumeId,
};
use btrfs_manager_core::rollback::{RollbackPlan, RollbackPrompt, RollbackStatus};
use chrono::Utc;
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn stage_rollback(
        &self,
        mountpoint: PathBuf,
        snapshot_path: PathBuf,
        return_snapshot_path: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&snapshot_path, "rollback snapshot path")?;
        validate_relative_btrfs_path(&return_snapshot_path, "rollback return snapshot path")?;
        let boot_integration = detect_boot_integration();
        let top = self.ensure_top_level_mount(&mountpoint)?;
        self.ensure_manager_subvolume_at_top_level(&top)?;
        let current_subvol = self
            .current_mounted_subvolume(&mountpoint)?
            .ok_or_else(|| {
                HelperError::InvalidPolicy("could not determine active root subvolume".into())
            })?;
        let current_root_abs = top.join(&current_subvol);
        let source_abs = top.join(&snapshot_path);
        let return_abs = top.join(&return_snapshot_path);
        let store = Self::state_store_at_top_level(&top)?;
        let plan = build_rollback_plan(
            mountpoint,
            snapshot_path.clone(),
            current_subvol.clone(),
            return_snapshot_path.clone(),
            boot_integration,
        );
        let plan_json = serde_json::to_vec_pretty(&plan)?;

        if let Some(parent) = return_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // mv @ → return anchor (rename in top-level tree; kernel VFS unaffected)
        std::fs::rename(&current_root_abs, &return_abs)?;

        // snapshot target → @ (fills freed slot; mv back on failure)
        let snap_result = self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "snapshot".into(),
                source_abs.display().to_string(),
                current_root_abs.display().to_string(),
            ],
        );
        if let Err(e) = snap_result {
            tracing::error!(error = %e, "snapshot failed, recovering @ via mv");
            self.recover_failed_rollback_stage(&current_root_abs, &return_abs)?;
            return Err(HelperError::InvalidPolicy(format!(
                "rollback failed (@ recovered): {e}"
            )));
        }

        self.persist_rollback_metadata_or_recover(
            &store,
            &top,
            &current_root_abs,
            &return_abs,
            &plan,
            &plan_json,
            &snapshot_path,
            &return_snapshot_path,
        )?;

        tracing::info!(replaced = %current_subvol.display(), "rollback staged — reboot to activate");
        Ok(HelperResponse {
            ok: true,
            message: format!(
                "rollback staged: {} replaced — reboot to activate",
                current_subvol.display()
            ),
            data: Some(serde_json::to_value(&plan)?),
        })
    }

    /// Writes the three pieces of rollback metadata (managed-snapshot row,
    /// rollback-plan row, rollback-plan file) as one unit. If any write
    /// fails, the freshly-created `@` subvolume is rolled back via
    /// `recover_failed_rollback_stage` so the filesystem and state never
    /// diverge.
    #[allow(clippy::too_many_arguments)]
    fn persist_rollback_metadata_or_recover(
        &self,
        store: &StateStore,
        top: &Path,
        current_root_abs: &Path,
        return_abs: &Path,
        plan: &RollbackPlan,
        plan_json: &[u8],
        snapshot_path: &Path,
        return_snapshot_path: &Path,
    ) -> Result<(), HelperError> {
        let metadata_result: Result<(), HelperError> = (|| {
            store.insert_managed_snapshot(
                None,
                &Snapshot {
                    id: Uuid::new_v4(),
                    source_subvolume: SubvolumeId(0),
                    path: return_snapshot_path.to_path_buf(),
                    created_at: Utc::now(),
                    tags: vec![
                        "rollback-anchor".into(),
                        format!("before-restoring:{}", snapshot_path.display()),
                    ],
                    origin: SnapshotOrigin::Managed,
                    state: SnapshotState::RollbackAnchor,
                },
            )?;
            store.insert_rollback_plan(plan)?;
            write_rollback_plan_file_data(top, plan.id, plan_json)?;
            Ok(())
        })();
        if let Err(err) = metadata_result {
            tracing::error!(error = %err, "rollback metadata failed, recovering @");
            self.recover_failed_rollback_stage(current_root_abs, return_abs)?;
            return Err(HelperError::InvalidPolicy(format!(
                "rollback metadata failed (@ recovered): {err}"
            )));
        }
        Ok(())
    }

    pub(crate) fn get_pending_rollback_response(&self) -> Result<HelperResponse, HelperError> {
        match self.pending_rollback()? {
            None => Ok(HelperResponse {
                ok: true,
                message: "no pending rollback".into(),
                data: None,
            }),
            Some(plan) => {
                let rebooted_since_staging = match (&plan.created_boot_id, current_boot_id()) {
                    (Some(staged_boot), Some(current_boot)) => staged_boot != &current_boot,
                    _ => false,
                };
                let prompt = RollbackPrompt {
                    plan,
                    rebooted_since_staging,
                };
                Ok(HelperResponse {
                    ok: true,
                    message: "pending rollback found".into(),
                    data: Some(serde_json::to_value(&prompt)?),
                })
            }
        }
    }

    pub(crate) fn commit_rollback(&self, plan_id: Uuid) -> Result<HelperResponse, HelperError> {
        let plan = self
            .pending_rollback()?
            .filter(|plan| plan.id == plan_id)
            .ok_or_else(|| {
                HelperError::InvalidPolicy(format!("no awaiting_reboot plan with id {plan_id}"))
            })?;
        let top = self.ensure_top_level_mount(&plan.mountpoint)?;
        self.ensure_manager_subvolume_at_top_level(&top)?;
        Self::state_store_at_top_level(&top)?.update_rollback_plan_status(plan_id, "activated")?;
        update_rollback_plan_file_status(&top, plan_id, RollbackStatus::Activated)?;
        tracing::info!(plan_id = %plan_id, "rollback committed");
        Ok(HelperResponse {
            ok: true,
            message: "rollback committed".into(),
            data: None,
        })
    }

    pub(crate) fn revert_rollback(&self, plan_id: Uuid) -> Result<HelperResponse, HelperError> {
        let plan = self
            .pending_rollback()?
            .filter(|p| p.id == plan_id)
            .ok_or_else(|| {
                HelperError::InvalidPolicy(format!("no awaiting_reboot plan with id {plan_id}"))
            })?;
        validate_relative_btrfs_path(&plan.replaced_subvol_path, "rollback replaced subvolume")?;
        validate_relative_btrfs_path(&plan.return_snapshot_path, "rollback return snapshot path")?;
        let top = self.ensure_top_level_mount(&plan.mountpoint)?;
        self.ensure_manager_subvolume_at_top_level(&top)?;
        let replaced_abs = top.join(&plan.replaced_subvol_path);
        let return_abs = top.join(&plan.return_snapshot_path);
        let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
        let discard_abs = top.join(format!("@btrfs-manager/discarded-{timestamp}"));
        if let Some(p) = discard_abs.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::rename(&replaced_abs, &discard_abs)?;
        if let Err(err) = std::fs::rename(&return_abs, &replaced_abs) {
            let recovery = std::fs::rename(&discard_abs, &replaced_abs);
            let message = match recovery {
                Ok(_) => format!(
                    "rollback revert failed before activation; current root restored: {err}"
                ),
                Err(recovery_err) => format!(
                    "rollback revert failed and current root recovery failed: {err}; recovery error: {recovery_err}"
                ),
            };
            return Err(HelperError::InvalidPolicy(message));
        }
        let store = Self::state_store_at_top_level(&top)?;
        store.update_rollback_plan_status(plan_id, "reverted")?;
        update_rollback_plan_file_status(&top, plan_id, RollbackStatus::Reverted)?;
        tracing::info!(plan_id = %plan_id, "rollback reverted");
        Ok(HelperResponse {
            ok: true,
            message: "rollback reverted — reboot to restore original root".into(),
            data: None,
        })
    }

    fn recover_failed_rollback_stage(
        &self,
        current_root_abs: &Path,
        return_abs: &Path,
    ) -> Result<(), HelperError> {
        if current_root_abs.exists() {
            self.runner.run(
                "btrfs",
                &[
                    "subvolume".into(),
                    "delete".into(),
                    current_root_abs.display().to_string(),
                ],
            )?;
        }
        std::fs::rename(return_abs, current_root_abs)?;
        Ok(())
    }

    pub(crate) fn pending_rollback(&self) -> Result<Option<RollbackPlan>, HelperError> {
        let top = match self.ensure_top_level_mount(Path::new("/")) {
            Ok(top) => top,
            Err(err) => {
                tracing::debug!(error = %err, "no top-level rollback plan fallback available");
                return Ok(None);
            }
        };
        self.ensure_manager_subvolume_at_top_level(&top)?;
        match read_rollback_plan_file_state(&top)? {
            RollbackPlanFileState::Pending(plan) => Ok(Some(plan)),
            RollbackPlanFileState::Resolved => Ok(None),
            RollbackPlanFileState::Missing => {
                Self::state_store_at_top_level(&top)?.get_pending_rollback()
            }
        }
    }

    /// Returns the subvolume path currently mounted at `mountpoint`, parsed
    /// from `findmnt` mount options. Only used by `stage_rollback` to find
    /// the active root subvolume to replace.
    fn current_mounted_subvolume(&self, mountpoint: &Path) -> Result<Option<PathBuf>, HelperError> {
        let options_output = self.runner.run(
            "findmnt",
            &[
                "-n".into(),
                "-o".into(),
                "OPTIONS".into(),
                "--target".into(),
                mountpoint.display().to_string(),
            ],
        )?;
        Ok(mounted_subvolume_from_options(options_output.trim()))
    }
}

fn build_rollback_plan(
    mountpoint: PathBuf,
    snapshot_path: PathBuf,
    current_subvol: PathBuf,
    return_snapshot_path: PathBuf,
    boot_integration: BootIntegration,
) -> RollbackPlan {
    let mut plan = RollbackPlan::new(
        mountpoint,
        snapshot_path.clone(),
        current_subvol,
        return_snapshot_path,
        boot_integration,
    );
    plan.created_boot_id = current_boot_id();
    plan.description = Some(format!("Before restoring {}", snapshot_path.display()));
    plan
}

pub(crate) fn rollback_plan_dir(top_level: &Path) -> PathBuf {
    top_level.join("@btrfs-manager").join("rollback-plans")
}

pub(crate) fn rollback_plan_file(top_level: &Path, plan_id: Uuid) -> PathBuf {
    rollback_plan_dir(top_level).join(format!("{plan_id}.json"))
}

pub(crate) fn write_rollback_plan_file(
    top_level: &Path,
    plan: &RollbackPlan,
) -> Result<(), HelperError> {
    let data = serde_json::to_vec_pretty(plan)?;
    write_rollback_plan_file_data(top_level, plan.id, &data)
}

pub(crate) fn write_rollback_plan_file_data(
    top_level: &Path,
    plan_id: Uuid,
    data: &[u8],
) -> Result<(), HelperError> {
    let dir = rollback_plan_dir(top_level);
    std::fs::create_dir_all(&dir)?;
    let path = rollback_plan_file(top_level, plan_id);
    std::fs::write(path, data)?;
    Ok(())
}

pub(crate) enum RollbackPlanFileState {
    Missing,
    Pending(RollbackPlan),
    Resolved,
}

pub(crate) fn read_rollback_plan_file_state(
    top_level: &Path,
) -> Result<RollbackPlanFileState, HelperError> {
    let dir = rollback_plan_dir(top_level);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RollbackPlanFileState::Missing);
        }
        Err(err) => return Err(err.into()),
    };

    let mut newest: Option<RollbackPlan> = None;
    let mut saw_plan_file = false;
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping unreadable rollback plan");
                continue;
            }
        };
        let plan: RollbackPlan = match serde_json::from_slice(&data) {
            Ok(plan) => plan,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping invalid rollback plan");
                continue;
            }
        };
        saw_plan_file = true;
        if !matches!(plan.status, RollbackStatus::AwaitingReboot) {
            continue;
        }
        if newest
            .as_ref()
            .is_none_or(|existing| existing.created_at < plan.created_at)
        {
            newest = Some(plan);
        }
    }
    if let Some(plan) = newest {
        Ok(RollbackPlanFileState::Pending(plan))
    } else if saw_plan_file {
        Ok(RollbackPlanFileState::Resolved)
    } else {
        Ok(RollbackPlanFileState::Missing)
    }
}

pub(crate) fn update_rollback_plan_file_status(
    top_level: &Path,
    plan_id: Uuid,
    status: RollbackStatus,
) -> Result<(), HelperError> {
    let path = rollback_plan_file(top_level, plan_id);
    if !path.exists() {
        return Ok(());
    }
    let data = std::fs::read(&path)?;
    let mut plan: RollbackPlan = serde_json::from_slice(&data)?;
    plan.status = status;
    write_rollback_plan_file(top_level, &plan)
}
