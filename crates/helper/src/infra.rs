use crate::state::StateStore;
use crate::validate::runtime_dir_from_env;
use crate::{CommandRunner, Helper, HelperError, HelperRequest, HelperResponse};
use std::path::{Path, PathBuf};

impl<R: CommandRunner> Helper<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            caller_uid: None,
        }
    }

    pub fn with_caller_uid(mut self, uid: u32) -> Self {
        self.caller_uid = Some(uid);
        self
    }

    pub(crate) fn managed_mount_roots(&self) -> Vec<PathBuf> {
        // Only browse (session-scoped) mounts are cleaned by CleanupManagedMounts.
        // Top-level mounts at /run/btrfs-manager/toplevel/ persist for the service
        // lifetime and are not cleaned here.
        let mut roots = Vec::new();
        if let Some(uid) = self.caller_uid {
            roots.push(PathBuf::from(format!("/run/user/{uid}/btrfs-manager")));
        }
        if let Some(runtime_dir) = runtime_dir_from_env() {
            let candidate = runtime_dir.join("btrfs-manager");
            if !roots.contains(&candidate) {
                roots.push(candidate);
            }
        }
        roots
    }

    /// Returns the persistent top-level (subvolid=5) mount path for the given
    /// mountpoint's filesystem. Mounts it at /run/btrfs-manager/toplevel/<uuid>/
    /// if not already mounted; subsequent calls are idempotent.
    pub(crate) fn ensure_top_level_mount(&self, mountpoint: &Path) -> Result<PathBuf, HelperError> {
        let uuid_output = self.runner.run(
            "findmnt",
            &[
                "-n".into(),
                "-o".into(),
                "UUID".into(),
                "--target".into(),
                mountpoint.display().to_string(),
            ],
        )?;
        let fs_uuid = uuid_output.trim().to_string();
        if fs_uuid.is_empty() {
            return Err(HelperError::InvalidPolicy(format!(
                "could not determine filesystem UUID for {}",
                mountpoint.display()
            )));
        }
        let base = std::env::var_os("BTRFS_MANAGER_TOPLEVEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/run/btrfs-manager/toplevel"));
        let top = base.join(&fs_uuid);
        // Idempotent: skip mount if already mounted at this path.
        let already = self
            .runner
            .run(
                "findmnt",
                &[
                    "-n".into(),
                    "--mountpoint".into(),
                    top.display().to_string(),
                ],
            )
            .ok()
            .map(|o| !o.trim().is_empty())
            .unwrap_or(false);
        if !already {
            let device_output = self.runner.run(
                "findmnt",
                &[
                    "-n".into(),
                    "-o".into(),
                    "SOURCE".into(),
                    "--target".into(),
                    mountpoint.display().to_string(),
                ],
            )?;
            let device = crate::subvolume::normalize_findmnt_source(device_output.trim());
            std::fs::create_dir_all(&top)?;
            self.runner.run(
                "mount",
                &[
                    "-o".into(),
                    "subvolid=5".into(),
                    device,
                    top.display().to_string(),
                ],
            )?;
            tracing::info!(path = %top.display(), "mounted btrfs top-level");
        }
        Ok(top)
    }

    pub(crate) fn state_store_for_mountpoint(
        &self,
        mountpoint: &Path,
    ) -> Result<StateStore, HelperError> {
        let top = self.ensure_top_level_mount(mountpoint)?;
        self.ensure_manager_subvolume_at_top_level(&top)?;
        Self::state_store_at_top_level(&top)
    }

    pub(crate) fn default_state_store(&self) -> Result<StateStore, HelperError> {
        self.state_store_for_mountpoint(Path::new("/"))
    }

    pub(crate) fn state_store_at_top_level(top_level: &Path) -> Result<StateStore, HelperError> {
        StateStore::open_at(state_store_path_at_top_level(top_level))
    }

    pub(crate) fn existing_state_store_at_top_level(
        top_level: &Path,
    ) -> Result<Option<StateStore>, HelperError> {
        let path = state_store_path_at_top_level(top_level);
        if path.exists() {
            StateStore::open_at(path).map(Some)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn ensure_manager_subvolume_at_top_level(
        &self,
        top_level: &Path,
    ) -> Result<(), HelperError> {
        let manager = top_level.join("@btrfs-manager");
        if !manager.exists() {
            self.runner.run(
                "btrfs",
                &[
                    "subvolume".into(),
                    "create".into(),
                    manager.display().to_string(),
                ],
            )?;
            return Ok(());
        }

        match self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "show".into(),
                manager.display().to_string(),
            ],
        ) {
            Ok(_) => Ok(()),
            Err(_) => Err(HelperError::InvalidPolicy(format!(
                "{} exists but is not a Btrfs subvolume",
                manager.display()
            ))),
        }
    }

    pub fn handle(&self, request: HelperRequest) -> Result<HelperResponse, HelperError> {
        match request {
            HelperRequest::DiscoverFilesystems => self.discover_filesystems(),
            HelperRequest::RunDiagnostics => self.run_diagnostics(),
            HelperRequest::ListSubvolumes { mountpoint } => self.list_subvolumes(mountpoint),
            HelperRequest::SnapshotDiskUsage {
                mountpoint,
                subvolume_path,
            } => self.snapshot_disk_usage_impl(mountpoint, subvolume_path),
            HelperRequest::CreateSnapshot {
                source,
                destination,
                readonly,
            } => self.create_snapshot_raw(source, destination, readonly),
            HelperRequest::DeleteSnapshot { path } => self.delete_snapshot_raw(path),
            HelperRequest::SetSnapshotReadOnly { path, readonly } => {
                self.set_snapshot_readonly_raw(path, readonly)
            }
            HelperRequest::MountSnapshot { source, target } => {
                self.mount_snapshot_bind_readonly(source, target)
            }
            HelperRequest::MountSubvolume {
                mountpoint,
                subvol_path,
                target,
            } => self.mount_subvolume_impl(mountpoint, subvol_path, target),
            HelperRequest::MountTopLevel { mountpoint } => self.mount_top_level_impl(mountpoint),
            HelperRequest::UnmountSnapshot { target } => self.unmount_snapshot_impl(target),
            HelperRequest::CleanupManagedMounts => self.cleanup_managed_mounts_impl(),
            HelperRequest::CreateManagedSnapshot {
                mountpoint,
                subvolume_path,
                snapshot_root,
                tags,
            } => self.create_managed_snapshot_impl(mountpoint, subvolume_path, snapshot_root, tags),
            HelperRequest::ListManagedSnapshots => self.list_managed_snapshots_impl(),
            HelperRequest::SetManagedSnapshotReadOnly {
                mountpoint,
                subvol_path,
                readonly,
            } => self.set_managed_snapshot_ro(mountpoint, subvol_path, readonly),
            HelperRequest::DeleteManagedSnapshot {
                mountpoint,
                subvolume_path,
            } => self.delete_managed_snapshot_impl(mountpoint, subvolume_path),
            HelperRequest::DeleteManagedSnapshots {
                mountpoint,
                subvolume_paths,
            } => self.delete_managed_snapshots_impl(mountpoint, subvolume_paths),
            HelperRequest::ListSnapshotPolicies => self.list_snapshot_policies_impl(),
            HelperRequest::UpsertSnapshotPolicy { policy } => {
                self.upsert_snapshot_policy_impl(policy)
            }
            HelperRequest::SetSnapshotPolicyEnabled { policy_id, enabled } => {
                self.set_snapshot_policy_enabled_impl(policy_id, enabled)
            }
            HelperRequest::PreviewRetention { policy_id } => self.preview_retention_impl(policy_id),
            HelperRequest::PreviewRetentionForPolicy { policy } => {
                self.preview_retention_for_policy_impl(policy)
            }
            HelperRequest::StageRollback {
                mountpoint,
                snapshot_path,
                return_snapshot_path,
            } => self.stage_rollback(mountpoint, snapshot_path, return_snapshot_path),
            HelperRequest::GetPendingRollback => self.get_pending_rollback_response(),
            HelperRequest::CommitRollback { plan_id } => self.commit_rollback(plan_id),
            HelperRequest::RevertRollback { plan_id } => self.revert_rollback(plan_id),
            HelperRequest::RunRetentionPolicy { policy_id } => {
                self.run_retention_policy_impl(policy_id)
            }
            HelperRequest::ListPolicyRunLogs { policy_id } => {
                self.list_policy_run_logs_impl(policy_id)
            }
            HelperRequest::OpenFileManager {
                path,
                display,
                wayland_display,
                xdg_runtime_dir,
            } => self.open_file_manager(path, display, wayland_display, xdg_runtime_dir),
            HelperRequest::ApplySelfUpdate {
                package_path,
                expected_sha256,
            } => self.apply_self_update_impl(package_path, expected_sha256),
        }
    }
}

pub(crate) fn state_store_path_at_top_level(top_level: &Path) -> PathBuf {
    top_level
        .join("@btrfs-manager")
        .join("state")
        .join("state.db")
}
