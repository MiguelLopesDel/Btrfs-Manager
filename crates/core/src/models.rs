use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilesystemId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubvolumeId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootIntegration {
    GrubBtrfs,
    RefindBtrfs,
    Conservative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filesystem {
    pub id: FilesystemId,
    pub devices: Vec<PathBuf>,
    pub mountpoints: Vec<PathBuf>,
    pub default_subvolume: Option<SubvolumeId>,
    pub boot_integration: BootIntegration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemMount {
    pub source: PathBuf,
    pub mountpoint: PathBuf,
    pub options: String,
    pub mounted_subvolume: Option<PathBuf>,
    pub is_active_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemSummary {
    pub id: FilesystemId,
    pub devices: Vec<PathBuf>,
    pub mounts: Vec<FilesystemMount>,
    pub default_subvolume: Option<SubvolumeId>,
    pub boot_integration: BootIntegration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SubvolumeKind {
    #[default]
    Normal,
    SnapshotContainer,
    Snapshot,
    ExternalSnapshot {
        tool: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subvolume {
    pub id: SubvolumeId,
    pub uuid: Option<Uuid>,
    pub parent_uuid: Option<Uuid>,
    pub path: PathBuf,
    #[serde(default)]
    pub kind: SubvolumeKind,
    pub mountpoint: Option<PathBuf>,
    pub readonly: bool,
    pub managed: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotOrigin {
    Managed,
    External { tool: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotState {
    ReadOnly,
    Unlocked,
    DirtyUnlocked,
    RollbackAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: Uuid,
    pub source_subvolume: SubvolumeId,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub origin: SnapshotOrigin,
    pub state: SnapshotState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicySchedule {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl PolicySchedule {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

impl std::str::FromStr for PolicySchedule {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "hourly" => Ok(Self::Hourly),
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            _ => Err(format!("unknown policy schedule: {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPolicy {
    pub id: Uuid,
    pub filesystem_id: Option<FilesystemId>,
    pub subvolume_id: SubvolumeId,
    pub source_path: PathBuf,
    pub mountpoint: PathBuf,
    pub snapshot_root: PathBuf,
    pub schedule: PolicySchedule,
    pub keep_hourly: usize,
    pub keep_daily: usize,
    pub keep_weekly: usize,
    pub keep_monthly: usize,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPreview {
    pub policy_id: Uuid,
    pub next_snapshot_path: PathBuf,
    pub delete: Vec<Snapshot>,
    pub keep: Vec<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyRunStatus {
    Success,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRunLog {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: PolicyRunStatus,
    pub created_snapshot: Option<PathBuf>,
    pub deleted_snapshots: Vec<PathBuf>,
    pub error: Option<String>,
}

impl Snapshot {
    pub fn is_managed(&self) -> bool {
        matches!(self.origin, SnapshotOrigin::Managed)
    }

    pub fn is_protected(&self) -> bool {
        matches!(
            self.state,
            SnapshotState::ReadOnly | SnapshotState::RollbackAnchor
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_subvolume_kind_when_deserializing_old_inventory() {
        let value = serde_json::json!({
            "id": 256,
            "uuid": null,
            "parent_uuid": null,
            "path": "@snapshots",
            "mountpoint": null,
            "readonly": false,
            "managed": false
        });
        let subvolume: Subvolume = serde_json::from_value(value).unwrap();
        assert_eq!(subvolume.kind, SubvolumeKind::Normal);
    }
}
