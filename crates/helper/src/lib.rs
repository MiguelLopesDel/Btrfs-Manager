use btrfs_manager_core::models::{FilesystemSummary, Snapshot, SnapshotPolicy, Subvolume};
use btrfs_manager_core::parser::ParseError;
use btrfs_manager_core::paths::PathSafetyError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;
use uuid::Uuid;

pub mod dbus;
mod state;

mod boot;
mod diagnostics;
mod infra;
mod mount_ops;
mod policy;
mod retention;
mod rollback;
mod snapshot;
mod subvolume;
mod validate;

#[derive(Debug, Error)]
pub enum HelperError {
    #[error("unsafe path: {0}")]
    UnsafePath(#[from] PathSafetyError),
    #[error("operation is not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error("failed to parse command output: {0}")]
    Parse(#[from] ParseError),
    #[error("failed to serialize helper response: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("state database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid policy: {0}")]
    InvalidPolicy(String),
    #[error("command failed: {program} {args:?}: {stderr}")]
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HelperRequest {
    DiscoverFilesystems,
    RunDiagnostics,
    ListSubvolumes {
        mountpoint: PathBuf,
    },
    CreateSnapshot {
        source: PathBuf,
        destination: PathBuf,
        readonly: bool,
    },
    DeleteSnapshot {
        path: PathBuf,
    },
    SetSnapshotReadOnly {
        path: PathBuf,
        readonly: bool,
    },
    MountSnapshot {
        source: PathBuf,
        target: PathBuf,
    },
    /// Mount a btrfs subvolume read-only by path relative to the filesystem root.
    /// Unlike MountSnapshot (which bind-mounts from the top-level mount and produces
    /// empty stubs for nested subvolumes), this does a real btrfs subvol mount so
    /// the full snapshot contents are visible.
    MountSubvolume {
        mountpoint: PathBuf,
        subvol_path: PathBuf,
        target: PathBuf,
    },
    MountTopLevel {
        mountpoint: PathBuf,
    },
    UnmountSnapshot {
        target: PathBuf,
    },
    CleanupManagedMounts,
    CreateManagedSnapshot {
        // filesystem mountpoint (e.g. "/")
        mountpoint: PathBuf,
        // subvolume path relative to the Btrfs volume root (e.g. "@cache", "@")
        subvolume_path: PathBuf,
        // container subvolume relative to the Btrfs volume root (e.g. "@snapshots")
        snapshot_root: PathBuf,
        tags: Vec<String>,
    },
    ListManagedSnapshots,
    /// Set ro flag on a managed snapshot and update its state in the DB.
    /// Rejects unlock for external snapshots (not in managed_snapshots).
    SetManagedSnapshotReadOnly {
        mountpoint: PathBuf,
        subvol_path: PathBuf,
        readonly: bool,
    },
    DeleteManagedSnapshot {
        // filesystem mountpoint (e.g. "/")
        mountpoint: PathBuf,
        // subvolume path relative to the Btrfs volume root (e.g. "@btrfs-manager/managed-...")
        subvolume_path: PathBuf,
    },
    /// Delete several managed snapshots in one authorized batch. Individual
    /// failures do not abort the batch; the response reports which paths failed.
    DeleteManagedSnapshots {
        // filesystem mountpoint (e.g. "/")
        mountpoint: PathBuf,
        // subvolume paths relative to the Btrfs volume root
        subvolume_paths: Vec<PathBuf>,
    },
    ListSnapshotPolicies,
    UpsertSnapshotPolicy {
        policy: SnapshotPolicy,
    },
    SetSnapshotPolicyEnabled {
        policy_id: Uuid,
        enabled: bool,
    },
    PreviewRetention {
        policy_id: Uuid,
    },
    PreviewRetentionForPolicy {
        policy: SnapshotPolicy,
    },
    /// Stage a rollback using the Timeshift method (no fstab or GRUB changes needed):
    /// snapshot current root as anchor, delete it from the namespace, snapshot target into
    /// the freed slot. Kernel keeps running on the old data; next boot uses the new subvol.
    StageRollback {
        mountpoint: PathBuf,
        snapshot_path: PathBuf,
        return_snapshot_path: PathBuf,
    },
    /// Return any rollback plan currently awaiting reboot, or None.
    GetPendingRollback,
    /// Accept the rollback after successful reboot (mark Activated).
    CommitRollback {
        plan_id: Uuid,
    },
    /// Cancel rollback before reboot, or revert after: restore original default subvolume.
    RevertRollback {
        plan_id: Uuid,
    },
    RunRetentionPolicy {
        policy_id: Uuid,
    },
    ListPolicyRunLogs {
        policy_id: Uuid,
    },
    /// Open a file manager as root, passing the calling user's display
    /// environment so the window appears on their desktop.
    OpenFileManager {
        path: PathBuf,
        display: String,
        wayland_display: String,
        xdg_runtime_dir: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubvolumeInventory {
    pub mountpoint: PathBuf,
    pub subvolumes: Vec<Subvolume>,
    pub snapshots: Vec<Snapshot>,
    /// Count of managed-snapshot DB rows pruned during this listing because their
    /// subvolume was deleted outside the app (e.g. `btrfs subvolume delete`).
    #[serde(default)]
    pub reconciled_external_deletions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemDiscovery {
    pub filesystems: Vec<FilesystemSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    pub generated_at: chrono::DateTime<Utc>,
    pub checks: Vec<DiagnosticCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub status: DiagnosticStatus,
    pub message: String,
    #[serde(default)]
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Default)]
struct RetentionRunOutcome {
    created_snapshot: Option<PathBuf>,
    deleted_snapshots: Vec<PathBuf>,
}

struct RetentionRunFailure {
    created_snapshot: Option<PathBuf>,
    deleted_snapshots: Vec<PathBuf>,
    error: HelperError,
}

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError>;
}

pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        let output = Command::new(program).args(args).output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(HelperError::CommandFailed {
                program: program.to_string(),
                args: args.to_vec(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        }
    }
}

pub struct Helper<R> {
    runner: R,
    caller_uid: Option<u32>,
}

#[cfg(test)]
mod tests;
