use super::connection::StateStore;
use crate::HelperError;
use btrfs_manager_core::models::BootIntegration;
use btrfs_manager_core::rollback::{RollbackPlan, RollbackStatus};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use std::path::PathBuf;
use uuid::Uuid;

impl StateStore {
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
