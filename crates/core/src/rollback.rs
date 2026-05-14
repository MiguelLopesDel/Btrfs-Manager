use crate::models::{BootIntegration, Snapshot};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollbackStatus {
    Prepared,
    AwaitingReboot,
    Activated,
    Reverted,
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackPlan {
    pub id: Uuid,
    pub source_snapshot: Snapshot,
    pub prepared_subvolume_path: PathBuf,
    pub return_snapshot_path: PathBuf,
    pub boot_integration: BootIntegration,
    pub status: RollbackStatus,
}

impl RollbackPlan {
    pub fn new(
        source_snapshot: Snapshot,
        prepared_subvolume_path: PathBuf,
        return_snapshot_path: PathBuf,
        boot_integration: BootIntegration,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_snapshot,
            prepared_subvolume_path,
            return_snapshot_path,
            boot_integration,
            status: RollbackStatus::Prepared,
        }
    }
}
