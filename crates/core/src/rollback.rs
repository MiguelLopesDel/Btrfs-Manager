use crate::models::BootIntegration;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollbackStatus {
    AwaitingReboot,
    Activated,
    Reverted,
    Failed { reason: String },
}

/// A staged rollback plan.
///
/// The mechanism (same as Timeshift): the active root subvolume (e.g. `@`) is
/// deleted from the Btrfs namespace and replaced by a snapshot of the target.
/// The kernel continues running on the old data (the VFS reference is alive);
/// on next boot the new `@` is mounted. No fstab or GRUB changes required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackPlan {
    pub id: Uuid,
    pub mountpoint: PathBuf,
    pub source_snapshot_path: PathBuf,
    /// The subvolume path that was replaced (e.g. `@`). Used to restore on revert.
    pub replaced_subvol_path: PathBuf,
    pub return_snapshot_path: PathBuf,
    pub boot_integration: BootIntegration,
    pub status: RollbackStatus,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub created_boot_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackPrompt {
    pub plan: RollbackPlan,
    pub rebooted_since_staging: bool,
}

impl RollbackPlan {
    pub fn new(
        mountpoint: PathBuf,
        source_snapshot_path: PathBuf,
        replaced_subvol_path: PathBuf,
        return_snapshot_path: PathBuf,
        boot_integration: BootIntegration,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            mountpoint,
            source_snapshot_path,
            replaced_subvol_path,
            return_snapshot_path,
            boot_integration,
            status: RollbackStatus::AwaitingReboot,
            created_at: Utc::now(),
            created_boot_id: None,
            description: None,
        }
    }
}
