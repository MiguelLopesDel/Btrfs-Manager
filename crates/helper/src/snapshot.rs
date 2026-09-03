use crate::state::StateStore;
use crate::validate::{validate_path, validate_relative_btrfs_path};
use crate::{CommandRunner, Helper, HelperError, HelperResponse};
use btrfs_manager_core::models::Snapshot;
use btrfs_manager_core::models::{SnapshotOrigin, SnapshotState, SubvolumeId};
use btrfs_manager_core::naming::snapshot_name_from_path;
use chrono::{Local, Utc};
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn list_managed_snapshots_impl(&self) -> Result<HelperResponse, HelperError> {
        let snapshots = self.default_state_store()?.list_all_managed_snapshots()?;
        Ok(HelperResponse {
            ok: true,
            message: format!("found {} managed snapshot(s)", snapshots.len()),
            data: Some(serde_json::to_value(&snapshots)?),
        })
    }

    pub(crate) fn delete_managed_snapshot_impl(
        &self,
        mountpoint: PathBuf,
        subvolume_path: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        // Validate the snapshot path before any privileged side effects
        // (top-level mount / @btrfs-manager creation): fail fast on an
        // unsafe path instead of mounting first and rejecting later.
        validate_relative_btrfs_path(&subvolume_path, "managed snapshot path")?;
        let store = self.state_store_for_mountpoint(&mountpoint)?;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        self.delete_managed_snapshot_at(&store, &top, &subvolume_path)?;
        Ok(HelperResponse {
            ok: true,
            message: format!("snapshot deleted: {}", subvolume_path.display()),
            data: None,
        })
    }

    /// Delete a single managed snapshot subvolume and its state row. If the
    /// subvolume is already gone from disk (e.g. removed with `btrfs subvolume
    /// delete` outside the app), the stale DB row is still cleaned up.
    pub(crate) fn delete_managed_snapshot_at(
        &self,
        store: &StateStore,
        top: &Path,
        subvolume_path: &Path,
    ) -> Result<(), HelperError> {
        validate_relative_btrfs_path(subvolume_path, "managed snapshot path")?;
        let snapshot = store.find_managed_snapshot_by_path(subvolume_path)?;
        if snapshot.state == SnapshotState::RollbackAnchor {
            return Err(HelperError::InvalidPolicy(format!(
                "{} is a rollback anchor and cannot be deleted directly — revert or commit the rollback first",
                subvolume_path.display()
            )));
        }
        let id = snapshot.id;
        let abs_path = top.join(subvolume_path);
        if abs_path.exists() {
            self.runner.run(
                "btrfs",
                &[
                    "subvolume".into(),
                    "delete".into(),
                    abs_path.display().to_string(),
                ],
            )?;
        } else {
            tracing::warn!(
                path = %subvolume_path.display(),
                "managed snapshot already missing on disk; cleaning stale state"
            );
        }
        store.delete_managed_snapshot(id)?;
        tracing::info!(path = %subvolume_path.display(), "managed snapshot deleted");
        Ok(())
    }

    /// Delete several managed snapshots in one authorized batch. Individual
    /// failures are collected and reported instead of aborting the batch.
    pub(crate) fn delete_managed_snapshots_impl(
        &self,
        mountpoint: PathBuf,
        subvolume_paths: Vec<PathBuf>,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        let store = self.state_store_for_mountpoint(&mountpoint)?;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        let mut deleted = 0usize;
        let mut failed: Vec<serde_json::Value> = Vec::new();
        for subvolume_path in &subvolume_paths {
            match self.delete_managed_snapshot_at(&store, &top, subvolume_path) {
                Ok(()) => deleted += 1,
                Err(err) => {
                    tracing::error!(
                        path = %subvolume_path.display(),
                        error = %err,
                        "batch delete: item failed"
                    );
                    failed.push(serde_json::json!({
                        "path": subvolume_path.display().to_string(),
                        "error": err.to_string(),
                    }));
                }
            }
        }
        let ok = failed.is_empty();
        let message = if ok {
            format!("{deleted} snapshot(s) deleted")
        } else {
            format!("{deleted} deleted, {} failed", failed.len())
        };
        Ok(HelperResponse {
            ok,
            message,
            data: Some(serde_json::json!({ "failed": failed })),
        })
    }

    pub(crate) fn create_managed_snapshot_impl(
        &self,
        mountpoint: PathBuf,
        subvolume_path: PathBuf,
        snapshot_root: PathBuf,
        tags: Vec<String>,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&subvolume_path, "snapshot source subvolume path")?;
        validate_relative_btrfs_path(&snapshot_root, "snapshot root")?;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        self.ensure_manager_subvolume_at_top_level(&top)?;
        let source = top.join(&subvolume_path);
        let dest_name = snapshot_name_from_path(&subvolume_path, Local::now());
        let dest_parent = top.join(&snapshot_root);
        if !dest_parent.exists() {
            self.runner.run(
                "btrfs",
                &[
                    "subvolume".into(),
                    "create".into(),
                    dest_parent.display().to_string(),
                ],
            )?;
        }
        let dest = dest_parent.join(&dest_name);
        self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "snapshot".into(),
                "-r".into(),
                source.display().to_string(),
                dest.display().to_string(),
            ],
        )?;
        let rel_path = snapshot_root.join(dest_name);
        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            source_subvolume: SubvolumeId(0),
            path: rel_path,
            created_at: Utc::now(),
            tags,
            origin: SnapshotOrigin::Managed,
            state: SnapshotState::ReadOnly,
        };
        Self::state_store_at_top_level(&top)?.insert_managed_snapshot(None, &snapshot)?;
        tracing::info!(path = %snapshot.path.display(), "managed snapshot created");
        Ok(HelperResponse {
            ok: true,
            message: format!("snapshot created at {}", snapshot.path.display()),
            data: Some(serde_json::to_value(&snapshot)?),
        })
    }

    pub(crate) fn set_managed_snapshot_ro(
        &self,
        mountpoint: PathBuf,
        subvol_path: PathBuf,
        readonly: bool,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&subvol_path, "managed snapshot path")?;
        let store = self.state_store_for_mountpoint(&mountpoint)?;
        let snapshot = store.find_managed_snapshot_by_path(&subvol_path)?;
        if snapshot.state == SnapshotState::RollbackAnchor {
            return Err(HelperError::InvalidPolicy(format!(
                "{} is a rollback anchor and cannot be locked/unlocked — its state is managed by the rollback lifecycle",
                subvol_path.display()
            )));
        }
        let id = snapshot.id;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        let abs_path = top.join(&subvol_path);
        let value = if readonly { "true" } else { "false" };
        self.runner.run(
            "btrfs",
            &[
                "property".into(),
                "set".into(),
                abs_path.display().to_string(),
                "ro".into(),
                value.into(),
            ],
        )?;
        let new_state = if readonly {
            SnapshotState::ReadOnly
        } else {
            SnapshotState::Unlocked
        };
        store.update_snapshot_state(id, &new_state)?;
        tracing::info!(path = %subvol_path.display(), readonly, "managed snapshot ro flag updated");
        Ok(HelperResponse {
            ok: true,
            message: format!("snapshot ro set to {value}"),
            data: None,
        })
    }
}
