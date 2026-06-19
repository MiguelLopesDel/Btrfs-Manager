use btrfs_manager_core::models::{
    BootIntegration, FilesystemId, FilesystemMount, FilesystemSummary, Snapshot, SnapshotOrigin,
    SnapshotPolicy, SnapshotState, Subvolume, SubvolumeId, SubvolumeKind,
};
use btrfs_manager_core::parser::{ParseError, parse_btrfs_subvolume_list, parse_findmnt_pairs};
use btrfs_manager_core::paths::{PathSafetyError, validate_absolute_no_traversal};
use btrfs_manager_core::retention::{RetentionPolicy, retention_keep_set};
use btrfs_manager_core::rollback::{RollbackPlan, RollbackPrompt, RollbackStatus};
use btrfs_manager_core::{PolicyRunLog, PolicyRunStatus, RetentionPreview};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;
use uuid::Uuid;

pub mod dbus;
mod state;
use state::StateStore;

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

    fn managed_mount_roots(&self) -> Vec<PathBuf> {
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
    fn ensure_top_level_mount(&self, mountpoint: &Path) -> Result<PathBuf, HelperError> {
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
            let device = normalize_findmnt_source(device_output.trim());
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

    fn state_store_for_mountpoint(&self, mountpoint: &Path) -> Result<StateStore, HelperError> {
        let top = self.ensure_top_level_mount(mountpoint)?;
        self.ensure_manager_subvolume_at_top_level(&top)?;
        Self::state_store_at_top_level(&top)
    }

    fn default_state_store(&self) -> Result<StateStore, HelperError> {
        self.state_store_for_mountpoint(Path::new("/"))
    }

    fn state_store_at_top_level(top_level: &Path) -> Result<StateStore, HelperError> {
        StateStore::open_at(state_store_path_at_top_level(top_level))
    }

    fn existing_state_store_at_top_level(
        top_level: &Path,
    ) -> Result<Option<StateStore>, HelperError> {
        let path = state_store_path_at_top_level(top_level);
        if path.exists() {
            StateStore::open_at(path).map(Some)
        } else {
            Ok(None)
        }
    }

    fn ensure_manager_subvolume_at_top_level(&self, top_level: &Path) -> Result<(), HelperError> {
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
            HelperRequest::CreateSnapshot {
                source,
                destination,
                readonly,
            } => {
                validate_path(&source)?;
                validate_path(&destination)?;
                let mut args = vec!["subvolume".into(), "snapshot".into()];
                if readonly {
                    args.push("-r".into());
                }
                args.push(source.display().to_string());
                args.push(destination.display().to_string());
                self.runner.run("btrfs", &args)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "snapshot created".into(),
                    data: None,
                })
            }
            HelperRequest::DeleteSnapshot { path } => {
                validate_path(&path)?;
                let args = vec![
                    "subvolume".into(),
                    "delete".into(),
                    path.display().to_string(),
                ];
                self.runner.run("btrfs", &args)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "snapshot deleted".into(),
                    data: None,
                })
            }
            HelperRequest::SetSnapshotReadOnly { path, readonly } => {
                validate_path(&path)?;
                let value = if readonly { "true" } else { "false" };
                let args = vec![
                    "property".into(),
                    "set".into(),
                    path.display().to_string(),
                    "ro".into(),
                    value.into(),
                ];
                self.runner.run("btrfs", &args)?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("readonly set to {value}"),
                    data: None,
                })
            }
            HelperRequest::MountSnapshot { source, target } => {
                validate_path(&source)?;
                validate_path(&target)?;
                tracing::debug!(
                    source = %source.display(),
                    target = %target.display(),
                    "mounting snapshot read-only"
                );
                let bind_args = vec![
                    "--bind".into(),
                    source.display().to_string(),
                    target.display().to_string(),
                ];
                self.runner.run("mount", &bind_args)?;
                let readonly_args = vec![
                    "-o".into(),
                    "remount,bind,ro".into(),
                    target.display().to_string(),
                ];
                self.runner.run("mount", &readonly_args)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "snapshot mounted".into(),
                    data: None,
                })
            }
            HelperRequest::MountSubvolume {
                mountpoint,
                subvol_path,
                target,
            } => self.mount_subvolume_impl(mountpoint, subvol_path, target),
            HelperRequest::MountTopLevel { mountpoint } => {
                validate_path(&mountpoint)?;
                let top = self.ensure_top_level_mount(&mountpoint)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "top-level ready".into(),
                    data: Some(serde_json::to_value(top)?),
                })
            }
            HelperRequest::UnmountSnapshot { target } => {
                validate_path(&target)?;
                tracing::debug!(target = %target.display(), "unmounting snapshot");
                let args = vec![target.display().to_string()];
                self.runner.run("umount", &args)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "snapshot unmounted".into(),
                    data: None,
                })
            }
            HelperRequest::CleanupManagedMounts => {
                let count = self.cleanup_managed_mounts()?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("cleaned up {count} managed mount(s)"),
                    data: None,
                })
            }
            HelperRequest::CreateManagedSnapshot {
                mountpoint,
                subvolume_path,
                snapshot_root,
                tags,
            } => self.create_managed_snapshot_impl(mountpoint, subvolume_path, snapshot_root, tags),
            HelperRequest::ListManagedSnapshots => {
                let snapshots = self.default_state_store()?.list_all_managed_snapshots()?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("found {} managed snapshot(s)", snapshots.len()),
                    data: Some(serde_json::to_value(&snapshots)?),
                })
            }
            HelperRequest::SetManagedSnapshotReadOnly {
                mountpoint,
                subvol_path,
                readonly,
            } => self.set_managed_snapshot_ro(mountpoint, subvol_path, readonly),
            HelperRequest::DeleteManagedSnapshot {
                mountpoint,
                subvolume_path,
            } => {
                validate_path(&mountpoint)?;
                validate_relative_btrfs_path(&subvolume_path, "managed snapshot path")?;
                let store = self.state_store_for_mountpoint(&mountpoint)?;
                let id = store.find_managed_snapshot_id_by_path(&subvolume_path)?;
                let top = self.ensure_top_level_mount(&mountpoint)?;
                let abs_path = top.join(&subvolume_path);
                self.runner.run(
                    "btrfs",
                    &[
                        "subvolume".into(),
                        "delete".into(),
                        abs_path.display().to_string(),
                    ],
                )?;
                store.delete_managed_snapshot(id)?;
                tracing::info!(path = %subvolume_path.display(), "managed snapshot deleted");
                Ok(HelperResponse {
                    ok: true,
                    message: format!("snapshot deleted: {}", subvolume_path.display()),
                    data: None,
                })
            }
            HelperRequest::ListSnapshotPolicies => {
                let policies = self.default_state_store()?.list_policies()?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("found {} snapshot policies", policies.len()),
                    data: Some(serde_json::to_value(policies)?),
                })
            }
            HelperRequest::UpsertSnapshotPolicy { policy } => {
                validate_policy(&policy)?;
                let store = self.default_state_store()?;
                let previous = store.get_policy(policy.id)?;
                store.upsert_policy(&policy)?;
                if let Err(err) = self.write_systemd_policy_units(&policy) {
                    self.recover_policy_after_scheduler_failure(&store, previous.as_ref(), &policy);
                    return Err(err);
                }
                Ok(HelperResponse {
                    ok: true,
                    message: "snapshot policy saved".into(),
                    data: Some(serde_json::to_value(policy)?),
                })
            }
            HelperRequest::SetSnapshotPolicyEnabled { policy_id, enabled } => {
                let store = self.default_state_store()?;
                let mut policy = store.get_policy(policy_id)?.ok_or_else(|| {
                    HelperError::InvalidPolicy(format!("unknown policy {policy_id}"))
                })?;
                let previous = policy.clone();
                policy.enabled = enabled;
                store.upsert_policy(&policy)?;
                if let Err(err) = self.write_systemd_policy_units(&policy) {
                    self.recover_policy_after_scheduler_failure(&store, Some(&previous), &policy);
                    return Err(err);
                }
                Ok(HelperResponse {
                    ok: true,
                    message: if enabled {
                        "snapshot policy enabled".into()
                    } else {
                        "snapshot policy disabled".into()
                    },
                    data: Some(serde_json::to_value(policy)?),
                })
            }
            HelperRequest::PreviewRetention { policy_id } => {
                let preview = self.retention_preview(policy_id)?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("{} snapshot(s) would be deleted", preview.delete.len()),
                    data: Some(serde_json::to_value(preview)?),
                })
            }
            HelperRequest::PreviewRetentionForPolicy { policy } => {
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
            HelperRequest::StageRollback {
                mountpoint,
                snapshot_path,
                return_snapshot_path,
            } => self.stage_rollback(mountpoint, snapshot_path, return_snapshot_path),
            HelperRequest::GetPendingRollback => self.get_pending_rollback_response(),
            HelperRequest::CommitRollback { plan_id } => self.commit_rollback(plan_id),
            HelperRequest::RevertRollback { plan_id } => self.revert_rollback(plan_id),
            HelperRequest::RunRetentionPolicy { policy_id } => {
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
            HelperRequest::ListPolicyRunLogs { policy_id } => {
                let logs = self.default_state_store()?.list_policy_logs(policy_id)?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("found {} policy run log(s)", logs.len()),
                    data: Some(serde_json::to_value(logs)?),
                })
            }
            HelperRequest::OpenFileManager {
                path,
                display,
                wayland_display,
                xdg_runtime_dir,
            } => self.open_file_manager(path, display, wayland_display, xdg_runtime_dir),
        }
    }

    fn run_diagnostics(&self) -> Result<HelperResponse, HelperError> {
        let mut checks = Vec::new();
        checks.push(diagnostic_check(
            "Helper",
            DiagnosticStatus::Ok,
            "Privileged helper responded to the diagnostics request",
            Vec::new(),
        ));

        checks.push(match self.runner.run("btrfs", &["version".into()]) {
            Ok(output) => diagnostic_check(
                "Btrfs tools",
                DiagnosticStatus::Ok,
                output.trim(),
                Vec::new(),
            ),
            Err(err) => diagnostic_check(
                "Btrfs tools",
                DiagnosticStatus::Error,
                "btrfs-progs is not available or failed to run",
                vec![err.to_string()],
            ),
        });

        checks.push(
            match self.runner.run(
                "systemctl",
                &["is-active".into(), "btrfs-manager-helper.service".into()],
            ) {
                Ok(output) if output.trim() == "active" => diagnostic_check(
                    "System service",
                    DiagnosticStatus::Ok,
                    "btrfs-manager-helper.service is active",
                    Vec::new(),
                ),
                Ok(output) => diagnostic_check(
                    "System service",
                    DiagnosticStatus::Warning,
                    "btrfs-manager-helper.service is installed but not active",
                    vec![format!("systemctl is-active returned {}", output.trim())],
                ),
                Err(err) => diagnostic_check(
                    "System service",
                    DiagnosticStatus::Warning,
                    "Could not query btrfs-manager-helper.service",
                    vec![err.to_string()],
                ),
            },
        );

        let polkit_path = Path::new("/usr/share/polkit-1/actions/org.btrfsmanager.helper.policy");
        checks.push(if polkit_path.exists() {
            diagnostic_check(
                "Polkit policy",
                DiagnosticStatus::Ok,
                "Polkit policy is installed",
                vec![polkit_path.display().to_string()],
            )
        } else {
            diagnostic_check(
                "Polkit policy",
                DiagnosticStatus::Warning,
                "Polkit policy file was not found",
                vec![polkit_path.display().to_string()],
            )
        });

        let root_info = self.runner.run(
            "findmnt",
            &[
                "-P".into(),
                "-n".into(),
                "-o".into(),
                "FSTYPE,SOURCE,TARGET,OPTIONS".into(),
                "--target".into(),
                "/".into(),
            ],
        );
        match root_info {
            Ok(output) => {
                let pairs = parse_findmnt_pairs(output.trim());
                let fstype = pairs.get("FSTYPE").map(String::as_str).unwrap_or("unknown");
                let source = pairs.get("SOURCE").map(String::as_str).unwrap_or("unknown");
                let options = pairs.get("OPTIONS").map(String::as_str).unwrap_or("");
                if fstype == "btrfs" {
                    let subvol = mounted_subvolume_from_options(options)
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "not reported".into());
                    checks.push(diagnostic_check(
                        "Root filesystem",
                        DiagnosticStatus::Ok,
                        "Root is mounted from Btrfs",
                        vec![format!("source: {source}"), format!("subvolume: {subvol}")],
                    ));
                } else {
                    checks.push(diagnostic_check(
                        "Root filesystem",
                        DiagnosticStatus::Error,
                        "Root is not mounted from Btrfs",
                        vec![format!("fstype: {fstype}"), format!("source: {source}")],
                    ));
                }
            }
            Err(err) => checks.push(diagnostic_check(
                "Root filesystem",
                DiagnosticStatus::Error,
                "Could not inspect the root filesystem",
                vec![err.to_string()],
            )),
        }

        match self.ensure_top_level_mount(Path::new("/")) {
            Ok(top) => {
                checks.push(diagnostic_check(
                    "Btrfs top-level mount",
                    DiagnosticStatus::Ok,
                    "Top-level Btrfs mount is available",
                    vec![top.display().to_string()],
                ));
                checks.extend(self.manager_state_diagnostics(&top));
                checks.extend(self.policy_timer_diagnostics(&top));
                checks.push(self.rollback_diagnostic(&top));
            }
            Err(err) => checks.push(diagnostic_check(
                "Btrfs top-level mount",
                DiagnosticStatus::Error,
                "Could not mount or find the Btrfs top-level subvolume",
                vec![err.to_string()],
            )),
        }

        let report = DiagnosticsReport {
            generated_at: Utc::now(),
            checks,
        };
        Ok(HelperResponse {
            ok: true,
            message: "diagnostics completed".into(),
            data: Some(serde_json::to_value(report)?),
        })
    }

    fn manager_state_diagnostics(&self, top: &Path) -> Vec<DiagnosticCheck> {
        let manager = top.join("@btrfs-manager");
        if !manager.exists() {
            return vec![diagnostic_check(
                "Manager state",
                DiagnosticStatus::Warning,
                "Manager state subvolume has not been initialized yet",
                vec![manager.display().to_string()],
            )];
        }
        let mut checks = Vec::new();
        match self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "show".into(),
                manager.display().to_string(),
            ],
        ) {
            Ok(_) => checks.push(diagnostic_check(
                "Manager state",
                DiagnosticStatus::Ok,
                "@btrfs-manager is a Btrfs subvolume",
                vec![manager.display().to_string()],
            )),
            Err(err) => checks.push(diagnostic_check(
                "Manager state",
                DiagnosticStatus::Error,
                "@btrfs-manager exists but is not a Btrfs subvolume",
                vec![err.to_string()],
            )),
        }
        let db_path = state_store_path_at_top_level(top);
        checks.push(if db_path.exists() {
            diagnostic_check(
                "State database",
                DiagnosticStatus::Ok,
                "State database exists",
                vec![db_path.display().to_string()],
            )
        } else {
            diagnostic_check(
                "State database",
                DiagnosticStatus::Warning,
                "State database has not been created yet",
                vec![db_path.display().to_string()],
            )
        });
        checks
    }

    fn policy_timer_diagnostics(&self, top: &Path) -> Vec<DiagnosticCheck> {
        let Ok(Some(store)) = Self::existing_state_store_at_top_level(top) else {
            return vec![diagnostic_check(
                "Snapshot policies",
                DiagnosticStatus::Warning,
                "No state database is available for policy checks",
                Vec::new(),
            )];
        };
        let policies = match store.list_policies() {
            Ok(policies) => policies,
            Err(err) => {
                return vec![diagnostic_check(
                    "Snapshot policies",
                    DiagnosticStatus::Error,
                    "Could not read snapshot policies",
                    vec![err.to_string()],
                )];
            }
        };
        if policies.is_empty() {
            return vec![diagnostic_check(
                "Snapshot policies",
                DiagnosticStatus::Ok,
                "No scheduled snapshot policies configured",
                Vec::new(),
            )];
        }
        let enabled = policies.iter().filter(|policy| policy.enabled).count();
        let mut details = vec![format!("{} policy(s), {enabled} enabled", policies.len())];
        let mut status = DiagnosticStatus::Ok;
        for policy in policies.iter().filter(|policy| policy.enabled) {
            let timer = format!("btrfs-manager-policy-{}.timer", policy.id);
            match self
                .runner
                .run("systemctl", &["is-enabled".into(), timer.clone()])
            {
                Ok(output) if output.trim() == "enabled" => {
                    details.push(format!("{timer}: enabled"));
                }
                Ok(output) => {
                    status = DiagnosticStatus::Warning;
                    details.push(format!("{timer}: {}", output.trim()));
                }
                Err(err) => {
                    status = DiagnosticStatus::Warning;
                    details.push(format!("{timer}: {err}"));
                }
            }
        }
        vec![diagnostic_check(
            "Snapshot policies",
            status,
            "Scheduled policy timers checked",
            details,
        )]
    }

    fn rollback_diagnostic(&self, top: &Path) -> DiagnosticCheck {
        match read_rollback_plan_file_state(top) {
            Ok(RollbackPlanFileState::Pending(plan)) => diagnostic_check(
                "Rollback",
                DiagnosticStatus::Warning,
                "A rollback is pending confirmation",
                vec![
                    format!("plan: {}", plan.id),
                    format!("source: {}", plan.source_snapshot_path.display()),
                    format!("return anchor: {}", plan.return_snapshot_path.display()),
                ],
            ),
            Ok(RollbackPlanFileState::Resolved | RollbackPlanFileState::Missing) => {
                diagnostic_check(
                    "Rollback",
                    DiagnosticStatus::Ok,
                    "No pending rollback",
                    Vec::new(),
                )
            }
            Err(err) => diagnostic_check(
                "Rollback",
                DiagnosticStatus::Error,
                "Could not read rollback state",
                vec![err.to_string()],
            ),
        }
    }

    fn list_subvolumes(&self, mountpoint: PathBuf) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        let output = self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "list".into(),
                "-u".into(),
                top.display().to_string(),
            ],
        )?;
        let mut subvolumes = parse_btrfs_subvolume_list(&output)?;
        classify_subvolumes(&mut subvolumes);
        if let Some(store) = Self::existing_state_store_at_top_level(&top)? {
            if let Ok(snapshots) = store.list_all_managed_snapshots() {
                for subvolume in &mut subvolumes {
                    if let Some(snap) = snapshots.iter().find(|s| s.path == subvolume.path) {
                        subvolume.managed = true;
                        subvolume.tags = snap.tags.clone();
                        subvolume.created_at = Some(snap.created_at);
                        subvolume.readonly = matches!(
                            snap.state,
                            SnapshotState::ReadOnly | SnapshotState::RollbackAnchor
                        );
                        subvolume.unlocked = !matches!(
                            snap.state,
                            SnapshotState::ReadOnly | SnapshotState::RollbackAnchor
                        );
                    }
                }
            }
        }
        let snapshots = snapshots_from_subvolumes(&subvolumes);
        let inventory = SubvolumeInventory {
            mountpoint,
            subvolumes,
            snapshots,
        };
        Ok(HelperResponse {
            ok: true,
            message: format!(
                "found {} subvolumes and {} snapshot candidates",
                inventory.subvolumes.len(),
                inventory.snapshots.len()
            ),
            data: Some(serde_json::to_value(inventory)?),
        })
    }

    fn mount_subvolume_impl(
        &self,
        mountpoint: PathBuf,
        subvol_path: PathBuf,
        target: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&subvol_path, "mount subvolume path")?;
        validate_mount_subvolume_option_path(&subvol_path)?;
        validate_path(&target)?;
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
        let device = normalize_findmnt_source(device_output.trim());
        std::fs::create_dir_all(&target)?;
        let subvol_opt = format!("subvol={}", subvol_path.display());
        self.runner.run(
            "mount",
            &[
                "-t".into(),
                "btrfs".into(),
                "-o".into(),
                subvol_opt,
                device,
                target.display().to_string(),
            ],
        )?;
        tracing::info!(
            subvol = %subvol_path.display(),
            target = %target.display(),
            "subvolume mounted"
        );
        Ok(HelperResponse {
            ok: true,
            message: "subvolume mounted".into(),
            data: None,
        })
    }

    fn create_managed_snapshot_impl(
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
        let timestamp = Utc::now().format("%Y-%m-%d_%H-%M-%S");
        let source_label = subvolume_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.trim_start_matches('@'))
            .filter(|s| !s.is_empty())
            .unwrap_or("root");
        let dest_name = format!("managed-{source_label}-{timestamp}");
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

    fn set_managed_snapshot_ro(
        &self,
        mountpoint: PathBuf,
        subvol_path: PathBuf,
        readonly: bool,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&subvol_path, "managed snapshot path")?;
        let store = self.state_store_for_mountpoint(&mountpoint)?;
        let id = store.find_managed_snapshot_id_by_path(&subvol_path)?;
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

    fn stage_rollback(
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
        let mut plan = RollbackPlan::new(
            mountpoint,
            snapshot_path.clone(),
            current_subvol.clone(),
            return_snapshot_path.clone(),
            boot_integration,
        );
        plan.created_boot_id = current_boot_id();
        plan.description = Some(format!("Before restoring {}", snapshot_path.display()));
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

        let metadata_result: Result<(), HelperError> = (|| {
            store.insert_managed_snapshot(
                None,
                &Snapshot {
                    id: Uuid::new_v4(),
                    source_subvolume: SubvolumeId(0),
                    path: return_snapshot_path.clone(),
                    created_at: Utc::now(),
                    tags: vec![
                        "rollback-anchor".into(),
                        format!("before-restoring:{}", snapshot_path.display()),
                    ],
                    origin: SnapshotOrigin::Managed,
                    state: SnapshotState::RollbackAnchor,
                },
            )?;
            store.insert_rollback_plan(&plan)?;
            write_rollback_plan_file_data(&top, plan.id, &plan_json)?;
            Ok(())
        })();
        if let Err(err) = metadata_result {
            tracing::error!(error = %err, "rollback metadata failed, recovering @");
            self.recover_failed_rollback_stage(&current_root_abs, &return_abs)?;
            return Err(HelperError::InvalidPolicy(format!(
                "rollback metadata failed (@ recovered): {err}"
            )));
        }
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

    fn get_pending_rollback_response(&self) -> Result<HelperResponse, HelperError> {
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

    fn commit_rollback(&self, plan_id: Uuid) -> Result<HelperResponse, HelperError> {
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

    fn revert_rollback(&self, plan_id: Uuid) -> Result<HelperResponse, HelperError> {
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

    fn pending_rollback(&self) -> Result<Option<RollbackPlan>, HelperError> {
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

    fn open_file_manager(
        &self,
        path: PathBuf,
        display: String,
        wayland_display: String,
        xdg_runtime_dir: String,
    ) -> Result<HelperResponse, HelperError> {
        validate_absolute_no_traversal(&path)?;
        let fm = [
            "/usr/bin/dolphin",
            "/usr/bin/nautilus",
            "/usr/bin/thunar",
            "/usr/bin/nemo",
        ]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no supported file manager found (tried dolphin, nautilus, thunar, nemo)",
            )
        })?;
        tracing::info!(path = %path.display(), fm, "opening file manager as root");
        std::process::Command::new(fm)
            .arg(&path)
            .env("DISPLAY", &display)
            .env("WAYLAND_DISPLAY", &wayland_display)
            .env("XDG_RUNTIME_DIR", &xdg_runtime_dir)
            .spawn()?;
        Ok(HelperResponse {
            ok: true,
            message: format!("file manager opened at {}", path.display()),
            data: None,
        })
    }

    fn cleanup_managed_mounts(&self) -> Result<usize, HelperError> {
        let mount_roots = self.managed_mount_roots();

        // List ALL current mounts and filter to managed roots.
        // Using --target with -R would find the parent mount of the root and list
        // all its submounts (potentially the entire system), so we list everything
        // and filter ourselves instead.
        let args = vec!["-n".into(), "-r".into(), "-o".into(), "TARGET".into()];
        let output = match self.runner.run("findmnt", &args) {
            Ok(o) => o,
            Err(HelperError::CommandFailed { .. }) => return Ok(0),
            Err(err) => return Err(err),
        };

        let mut targets: Vec<PathBuf> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PathBuf::from(line.trim()))
            .filter(|path| mount_roots.iter().any(|root| path.starts_with(root)))
            .collect();

        targets.sort_by_key(|target| std::cmp::Reverse(target.as_os_str().len()));
        targets.dedup();

        let mut cleaned = 0;
        for target in targets {
            tracing::debug!(target = %target.display(), "unmounting managed browse mount");
            let args = vec![target.display().to_string()];
            self.runner.run("umount", &args)?;
            cleaned += 1;
        }

        Ok(cleaned)
    }

    fn discover_filesystems(&self) -> Result<HelperResponse, HelperError> {
        let args = vec![
            "-P".into(),
            "-t".into(),
            "btrfs".into(),
            "-o".into(),
            "UUID,SOURCE,TARGET,OPTIONS".into(),
        ];
        let output = self.runner.run("findmnt", &args)?;
        let mut filesystems: BTreeMap<Uuid, FilesystemSummary> = BTreeMap::new();

        for line in output.lines().filter(|line| !line.trim().is_empty()) {
            let pairs = parse_findmnt_pairs(line);
            let Some(uuid) = pairs
                .get("UUID")
                .and_then(|value| Uuid::parse_str(value).ok())
            else {
                continue;
            };
            let Some(target) = pairs.get("TARGET").map(PathBuf::from) else {
                continue;
            };
            let source = pairs
                .get("SOURCE")
                .map(|value| PathBuf::from(normalize_findmnt_source(value)))
                .unwrap_or_default();
            let options = pairs.get("OPTIONS").cloned().unwrap_or_default();
            let mounted_subvolume = mounted_subvolume_from_options(&options);
            let default_subvolume = self.default_subvolume_for_mount(&target).ok().flatten();
            let boot_integration = detect_boot_integration();

            let entry = filesystems
                .entry(uuid)
                .or_insert_with(|| FilesystemSummary {
                    id: FilesystemId(uuid),
                    devices: Vec::new(),
                    mounts: Vec::new(),
                    default_subvolume: default_subvolume.clone(),
                    boot_integration,
                });
            if !source.as_os_str().is_empty() && !entry.devices.contains(&source) {
                entry.devices.push(source.clone());
            }
            if entry.default_subvolume.is_none() {
                entry.default_subvolume = default_subvolume;
            }
            entry.mounts.push(FilesystemMount {
                source,
                mountpoint: target.clone(),
                options,
                mounted_subvolume,
                is_active_root: target == Path::new("/"),
            });
        }

        let discovery = FilesystemDiscovery {
            filesystems: filesystems.into_values().collect(),
        };
        Ok(HelperResponse {
            ok: true,
            message: format!("found {} Btrfs filesystem(s)", discovery.filesystems.len()),
            data: Some(serde_json::to_value(discovery)?),
        })
    }

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

    fn default_subvolume_for_mount(
        &self,
        mountpoint: &Path,
    ) -> Result<Option<SubvolumeId>, HelperError> {
        let args = vec![
            "subvolume".into(),
            "get-default".into(),
            mountpoint.display().to_string(),
        ];
        match self.runner.run("btrfs", &args) {
            Ok(output) => Ok(parse_default_subvolume_id(&output)),
            Err(HelperError::CommandFailed { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn retention_preview(&self, policy_id: Uuid) -> Result<RetentionPreview, HelperError> {
        let default_store = self.default_state_store()?;
        let policy = default_store
            .get_policy(policy_id)?
            .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
        let store = self.state_store_for_mountpoint(&policy.mountpoint)?;
        let snapshots = store.list_managed_snapshots_for_policy(policy_id)?;
        Ok(retention_preview_for_policy(&policy, &snapshots))
    }

    fn run_retention_policy(&self, policy_id: Uuid) -> Result<PolicyRunLog, HelperError> {
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
        let result: Result<(), HelperError> = (|| {
            let default_store = self.default_state_store()?;
            let policy = default_store
                .get_policy(policy_id)?
                .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
            if !policy.enabled {
                return Err(HelperError::InvalidPolicy(format!(
                    "policy {policy_id} is disabled"
                )));
            }
            validate_policy(&policy)?;

            let top = self.ensure_top_level_mount(&policy.mountpoint)?;
            let store = Self::state_store_at_top_level(&top)?;
            let existing = store.list_managed_snapshots_for_policy(policy_id)?;
            let keep =
                retention_keep_set(&existing, &retention_policy_from_snapshot_policy(&policy));
            let delete_candidates: Vec<_> = existing
                .into_iter()
                .filter(|snapshot| {
                    !keep.contains(&snapshot.id)
                        && snapshot.is_managed()
                        && snapshot.state != SnapshotState::RollbackAnchor
                })
                .collect();

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
            let snap_dir_abs = top.join(policy_snapshot_dir(&policy));
            std::fs::create_dir_all(&snap_dir_abs)?;

            let dest_name = format!(
                "{}-{}",
                sanitize_snapshot_label(&policy.source_path),
                Utc::now().format("%Y%m%d-%H%M%S")
            );
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
            let rel_path = policy_snapshot_dir(&policy).join(&dest_name);
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
                match self.delete_retention_snapshot_subvolume(&top, &rel_path) {
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

            // Retention deletes are based on the same pre-run snapshot set used
            // by preview, so a run does not delete more than preview showed.
            for old in delete_candidates {
                validate_relative_btrfs_path(&old.path, "managed snapshot path from state")?;
                let deleted = match self.delete_retention_snapshot_subvolume(&top, &old.path) {
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
        })();
        match result {
            Ok(()) => Ok(outcome),
            Err(error) => Err(RetentionRunFailure {
                created_snapshot: outcome.created_snapshot,
                deleted_snapshots: outcome.deleted_snapshots,
                error,
            }),
        }
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

    fn recover_policy_after_scheduler_failure(
        &self,
        store: &StateStore,
        previous: Option<&SnapshotPolicy>,
        attempted: &SnapshotPolicy,
    ) {
        let restore_result = match previous {
            Some(policy) => store.upsert_policy(policy),
            None => store.delete_policy(attempted.id),
        };
        if let Err(err) = restore_result {
            tracing::error!(
                policy_id = %attempted.id,
                error = %err,
                "failed to restore policy state after scheduler setup failure"
            );
        }

        let unit_result = match previous {
            Some(policy) => self.write_systemd_policy_units(policy),
            None => self.remove_systemd_policy_units(attempted.id),
        };
        if let Err(err) = unit_result {
            tracing::warn!(
                policy_id = %attempted.id,
                error = %err,
                "failed to restore scheduler unit files after scheduler setup failure"
            );
        }
    }

    fn write_systemd_policy_units(&self, policy: &SnapshotPolicy) -> Result<(), HelperError> {
        let unit_dir = systemd_unit_dir();
        std::fs::create_dir_all(&unit_dir)?;
        let service_path = unit_dir.join(format!("btrfs-manager-policy-{}.service", policy.id));
        let timer_path = unit_dir.join(format!("btrfs-manager-policy-{}.timer", policy.id));
        let service_name = service_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| HelperError::InvalidPolicy("invalid service unit name".into()))?;
        let timer_name = timer_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| HelperError::InvalidPolicy("invalid timer unit name".into()))?;

        std::fs::write(
            &service_path,
            format!(
                "[Unit]\nDescription=Btrfs Manager snapshot policy {}\n\n[Service]\nType=oneshot\nExecStart=/usr/lib/btrfs-manager/btrfs-manager-helper run-retention-policy --policy-id {}\n",
                policy.id, policy.id
            ),
        )?;
        std::fs::write(
            &timer_path,
            format!(
                "[Unit]\nDescription=Btrfs Manager scheduled snapshot policy {}\n\n[Timer]\nOnCalendar={}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
                policy.id,
                policy.schedule.as_str()
            ),
        )?;

        self.runner.run("systemctl", &["daemon-reload".into()])?;
        if policy.enabled {
            self.runner.run(
                "systemctl",
                &["enable".into(), "--now".into(), timer_name.into()],
            )?;
        } else {
            self.runner.run(
                "systemctl",
                &["disable".into(), "--now".into(), timer_name.into()],
            )?;
        }
        let _ = service_name;
        Ok(())
    }

    fn remove_systemd_policy_units(&self, policy_id: Uuid) -> Result<(), HelperError> {
        let unit_dir = systemd_unit_dir();
        for extension in ["service", "timer"] {
            let path = unit_dir.join(format!("btrfs-manager-policy-{policy_id}.{extension}"));
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        self.runner.run("systemctl", &["daemon-reload".into()])?;
        Ok(())
    }
}

fn systemd_unit_dir() -> PathBuf {
    std::env::var_os("BTRFS_MANAGER_SYSTEMD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/systemd/system"))
}

fn diagnostic_check(
    name: impl Into<String>,
    status: DiagnosticStatus,
    message: impl Into<String>,
    details: Vec<String>,
) -> DiagnosticCheck {
    DiagnosticCheck {
        name: name.into(),
        status,
        message: message.into(),
        details,
    }
}

fn state_store_path_at_top_level(top_level: &Path) -> PathBuf {
    top_level
        .join("@btrfs-manager")
        .join("state")
        .join("state.db")
}

fn validate_policy(policy: &SnapshotPolicy) -> Result<(), HelperError> {
    // source_path and snapshot_root are relative to the Btrfs volume root.
    // mountpoint is an absolute path used to find the block device.
    validate_path(&policy.mountpoint)?;
    validate_relative_btrfs_path(&policy.source_path, "policy source path")?;
    validate_relative_btrfs_path(&policy.snapshot_root, "policy snapshot root")?;
    if policy.keep_hourly + policy.keep_daily + policy.keep_weekly + policy.keep_monthly == 0 {
        return Err(HelperError::InvalidPolicy(
            "at least one retention bucket must be kept".into(),
        ));
    }
    Ok(())
}

fn retention_policy_from_snapshot_policy(policy: &SnapshotPolicy) -> RetentionPolicy {
    RetentionPolicy {
        hourly: policy.keep_hourly,
        daily: policy.keep_daily,
        weekly: policy.keep_weekly,
        monthly: policy.keep_monthly,
    }
}

fn retention_preview_for_policy(
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
        next_snapshot_path: policy_snapshot_dir(policy).join(format!(
            "{}-{}",
            sanitize_snapshot_label(&policy.source_path),
            Utc::now().format("%Y%m%d-%H%M%S")
        )),
        delete,
        keep,
    }
}

// Returns a path relative to the Btrfs volume root (used for SQLite storage
// and preview display). Callers that need an absolute path join with the
// top-level mount point.
fn policy_snapshot_dir(policy: &SnapshotPolicy) -> PathBuf {
    policy.snapshot_root.join(policy.id.to_string())
}

fn sanitize_snapshot_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("subvolume")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn validate_path(path: &Path) -> Result<(), HelperError> {
    validate_absolute_no_traversal(path).map_err(HelperError::from)
}

fn validate_relative_btrfs_path(path: &Path, label: &str) -> Result<(), HelperError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(HelperError::InvalidPolicy(format!(
            "{label} must be relative to the Btrfs top-level and must not contain traversal: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_mount_subvolume_option_path(path: &Path) -> Result<(), HelperError> {
    if path.to_string_lossy().contains(',') {
        return Err(HelperError::InvalidPolicy(format!(
            "mount subvolume path must not contain commas: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
fn validate_managed_mount_target_with_roots(
    path: &Path,
    roots: &[PathBuf],
) -> Result<(), HelperError> {
    if roots.iter().any(|root| path.starts_with(root)) {
        Ok(())
    } else {
        Err(PathSafetyError::Traversal.into())
    }
}

fn runtime_dir_from_env() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(value));
    }
    for key in ["PKEXEC_UID", "SUDO_UID", "UID"] {
        if let Some(uid) = std::env::var_os(key).and_then(|value| value.into_string().ok()) {
            if uid.chars().all(|character| character.is_ascii_digit()) {
                return Some(PathBuf::from("/run/user").join(uid));
            }
        }
    }
    None
}

fn normalize_findmnt_source(source: &str) -> String {
    source
        .split_once('[')
        .map(|(device, _)| device)
        .unwrap_or(source)
        .to_string()
}

fn mounted_subvolume_from_options(options: &str) -> Option<PathBuf> {
    options.split(',').find_map(|option| {
        option
            .strip_prefix("subvol=")
            .map(|subvolume| PathBuf::from(subvolume.trim_start_matches('/')))
    })
}

fn parse_default_subvolume_id(output: &str) -> Option<SubvolumeId> {
    let mut tokens = output.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "ID" {
            return tokens.next()?.parse::<u64>().ok().map(SubvolumeId);
        }
    }
    None
}

fn detect_boot_integration() -> BootIntegration {
    if Path::new("/etc/default/grub-btrfs/config").exists()
        || Path::new("/etc/grub.d/41_snapshots-btrfs").exists()
    {
        BootIntegration::GrubBtrfs
    } else if Path::new("/boot/refind_linux.conf").exists()
        || Path::new("/boot/efi/EFI/refind/refind.conf").exists()
    {
        BootIntegration::RefindBtrfs
    } else {
        BootIntegration::Conservative
    }
}

fn current_boot_id() -> Option<String> {
    if let Ok(value) = std::env::var("BTRFS_MANAGER_BOOT_ID") {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn rollback_plan_dir(top_level: &Path) -> PathBuf {
    top_level.join("@btrfs-manager").join("rollback-plans")
}

fn rollback_plan_file(top_level: &Path, plan_id: Uuid) -> PathBuf {
    rollback_plan_dir(top_level).join(format!("{plan_id}.json"))
}

fn write_rollback_plan_file(top_level: &Path, plan: &RollbackPlan) -> Result<(), HelperError> {
    let data = serde_json::to_vec_pretty(plan)?;
    write_rollback_plan_file_data(top_level, plan.id, &data)
}

fn write_rollback_plan_file_data(
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

enum RollbackPlanFileState {
    Missing,
    Pending(RollbackPlan),
    Resolved,
}

fn read_rollback_plan_file_state(top_level: &Path) -> Result<RollbackPlanFileState, HelperError> {
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

fn update_rollback_plan_file_status(
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

fn snapshots_from_subvolumes(subvolumes: &[Subvolume]) -> Vec<Snapshot> {
    subvolumes
        .iter()
        .filter(|subvolume| {
            matches!(
                subvolume.kind,
                SubvolumeKind::Snapshot | SubvolumeKind::ExternalSnapshot { .. }
            )
        })
        .map(|subvolume| Snapshot {
            id: subvolume.uuid.unwrap_or_else(Uuid::new_v4),
            source_subvolume: subvolume.id.clone(),
            path: subvolume.path.clone(),
            created_at: Utc::now(),
            tags: Vec::new(),
            origin: SnapshotOrigin::External {
                tool: match &subvolume.kind {
                    SubvolumeKind::ExternalSnapshot { tool } => tool.clone(),
                    _ => None,
                },
            },
            state: SnapshotState::ReadOnly,
        })
        .collect()
}

fn classify_subvolumes(subvolumes: &mut [Subvolume]) {
    for subvolume in subvolumes {
        subvolume.kind = classify_subvolume_kind(&subvolume.path);
    }
}

fn classify_subvolume_kind(path: &Path) -> SubvolumeKind {
    if path_looks_like_snapshot_container(path) {
        return SubvolumeKind::SnapshotContainer;
    }

    if path_looks_like_snapshot(path) {
        let tool = detect_snapshot_tool(path);
        if tool.is_some() {
            SubvolumeKind::ExternalSnapshot { tool }
        } else {
            SubvolumeKind::Snapshot
        }
    } else {
        SubvolumeKind::Normal
    }
}

fn path_looks_like_snapshot_container(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "@snapshots"
            | ".snapshots"
            | "snapshots"
            | "snapper"
            | "timeshift"
            | "timeshift-btrfs"
            | "btrfs-manager-snapshots"
            | "@btrfs-manager"
    )
}

fn path_looks_like_snapshot(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("timeshift")
        || text.contains("snapper")
        || text.contains("snapshots/")
        || text.contains(".snapshots/")
        || text.ends_with("/snapshot")
        || text.contains("btrfs-manager/")
}

fn detect_snapshot_tool(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().to_ascii_lowercase();
    if text.contains("timeshift") {
        return Some("timeshift".into());
    }
    if text.contains("snapper") {
        return Some("snapper".into());
    }
    // Snapper structural pattern: <snapshots_container>/<numeric_id>/snapshot
    // e.g. @snapshots/265/snapshot, .snapshots/1/snapshot, @home/.snapshots/3/snapshot
    // The tool name "snapper" never appears in these paths — match by structure instead.
    if looks_like_snapper_snapshot(path) {
        return Some("snapper".into());
    }
    None
}

// Returns true for paths matching Snapper's convention:
//   <any_prefix>/<snapshots_container>/<numeric_id>/snapshot
// where <snapshots_container> ends with "snapshots" (e.g. @snapshots, .snapshots).
fn looks_like_snapper_snapshot(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    let n = components.len();
    if n < 3 {
        return false;
    }
    let leaf = components[n - 1].as_os_str().to_str().unwrap_or("");
    if leaf != "snapshot" {
        return false;
    }
    let id = components[n - 2].as_os_str().to_str().unwrap_or("");
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let container = components[n - 3]
        .as_os_str()
        .to_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    // Accept @snapshots, .snapshots, snapshots (with or without sigil prefix)
    container.ends_with("snapshots")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::cell::RefCell;

    struct RecordingRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            if program == "btrfs" && args.first().map(String::as_str) == Some("subvolume") {
                if args.get(1).map(String::as_str) == Some("get-default") {
                    Ok("ID 256 gen 10 top level 5 path @\n".into())
                } else {
                    Ok("ID 256 gen 10 top level 5 uuid db14ad1b-c411-f247-8770-e8386e647b88 path @data\nID 257 gen 9 top level 5 uuid 1a123437-56b1-a849-b390-b5ec0c89c707 path snapshots/snap-1\n".into())
                }
            } else if program == "findmnt" && args.iter().any(|arg| arg == "TARGET") {
                Ok(format!(
                    "{}\n",
                    std::env::temp_dir()
                        .join("btrfs-manager-browse")
                        .join("snapshot-296-test")
                        .display()
                ))
            } else if program == "findmnt"
                && args.iter().any(|arg| arg == "UUID,SOURCE,TARGET,OPTIONS")
            {
                Ok("UUID=\"550e8400-e29b-41d4-a716-446655440000\" SOURCE=\"/dev/mapper/cryptroot[/@]\" TARGET=\"/\" OPTIONS=\"rw,relatime,subvol=/@\"\nUUID=\"550e8400-e29b-41d4-a716-446655440000\" SOURCE=\"/dev/mapper/cryptroot[/@home]\" TARGET=\"/home\" OPTIONS=\"rw,relatime,subvol=/@home\"\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
                // Simulate "not mounted" so ensure_top_level_mount always mounts.
                Ok("".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
                Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
            } else if program == "findmnt" {
                Ok("/dev/mapper/cryptroot[/@]\n".into())
            } else {
                Ok("ok".into())
            }
        }
    }

    #[test]
    fn creates_readonly_snapshot_with_allowlisted_command_shape() {
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        helper
            .handle(HelperRequest::CreateSnapshot {
                source: "/mnt/@".into(),
                destination: "/mnt/@snapshots/one".into(),
                readonly: true,
            })
            .unwrap();
        let calls = helper.runner.calls.borrow();
        assert_eq!(calls[0].0, "btrfs");
        assert_eq!(calls[0].1[0..3], ["subvolume", "snapshot", "-r"]);
    }

    #[test]
    fn list_subvolumes_returns_structured_inventory() {
        let tmp = std::env::temp_dir().join("btrfs-manager-test-toplevel");
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &tmp);
        }
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        let response = helper
            .handle(HelperRequest::ListSubvolumes {
                mountpoint: "/mnt".into(),
            })
            .unwrap();
        assert!(response.ok);
        assert!(response.data.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn classifies_snapshot_containers_separately_from_snapshots() {
        // Containers
        assert_eq!(
            classify_subvolume_kind(Path::new("@snapshots")),
            SubvolumeKind::SnapshotContainer
        );
        assert_eq!(
            classify_subvolume_kind(Path::new(".snapshots")),
            SubvolumeKind::SnapshotContainer
        );
        assert_eq!(
            classify_subvolume_kind(Path::new("timeshift-btrfs")),
            SubvolumeKind::SnapshotContainer
        );

        // Snapper: @snapshots/<numeric_id>/snapshot pattern
        assert_eq!(
            classify_subvolume_kind(Path::new("@snapshots/296/snapshot")),
            SubvolumeKind::ExternalSnapshot {
                tool: Some("snapper".into())
            }
        );
        assert_eq!(
            classify_subvolume_kind(Path::new("@snapshots/265/snapshot")),
            SubvolumeKind::ExternalSnapshot {
                tool: Some("snapper".into())
            }
        );
        // Snapper: .snapshots/<id>/snapshot (openSUSE default, or Arch without @)
        assert_eq!(
            classify_subvolume_kind(Path::new(".snapshots/1/snapshot")),
            SubvolumeKind::ExternalSnapshot {
                tool: Some("snapper".into())
            }
        );
        // Snapper: nested inside another subvolume (@home/.snapshots/<id>/snapshot)
        assert_eq!(
            classify_subvolume_kind(Path::new("@home/.snapshots/3/snapshot")),
            SubvolumeKind::ExternalSnapshot {
                tool: Some("snapper".into())
            }
        );

        // Timeshift: timeshift-btrfs/snapshots/<date>/@
        assert_eq!(
            classify_subvolume_kind(Path::new("timeshift-btrfs/snapshots/2024-01-01_12-00-00/@")),
            SubvolumeKind::ExternalSnapshot {
                tool: Some("timeshift".into())
            }
        );
        assert_eq!(
            classify_subvolume_kind(Path::new("timeshift-btrfs/snapshots/one")),
            SubvolumeKind::ExternalSnapshot {
                tool: Some("timeshift".into())
            }
        );

        // Our managed snapshots under @btrfs-manager
        assert_eq!(
            classify_subvolume_kind(Path::new("@btrfs-manager/managed-2026-05-21_16-34-31")),
            SubvolumeKind::Snapshot
        );

        // Normal subvolumes — must NOT be misclassified
        assert_eq!(
            classify_subvolume_kind(Path::new("@home")),
            SubvolumeKind::Normal
        );
        assert_eq!(
            classify_subvolume_kind(Path::new("@")),
            SubvolumeKind::Normal
        );
        // "snapshot" leaf but non-numeric parent → not Snapper
        assert_eq!(
            classify_subvolume_kind(Path::new("backups/important/snapshot")),
            SubvolumeKind::Snapshot
        );
    }

    #[test]
    fn discovers_btrfs_filesystems_from_findmnt_pairs() {
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        let response = helper.handle(HelperRequest::DiscoverFilesystems).unwrap();
        let discovery: FilesystemDiscovery =
            serde_json::from_value(response.data.unwrap()).unwrap();
        assert_eq!(discovery.filesystems.len(), 1);
        let fs = &discovery.filesystems[0];
        assert_eq!(fs.mounts.len(), 2);
        assert_eq!(fs.devices, vec![PathBuf::from("/dev/mapper/cryptroot")]);
        assert_eq!(fs.default_subvolume, Some(SubvolumeId(256)));
        assert!(fs.mounts.iter().any(|mount| mount.is_active_root));
        assert_eq!(fs.mounts[0].mounted_subvolume, Some(PathBuf::from("@")));
    }

    #[test]
    fn mounts_top_level_with_subvolid_five() {
        let tmp = std::env::temp_dir().join("btrfs-manager-test-toplevel2");
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &tmp);
        }
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        let response = helper
            .handle(HelperRequest::MountTopLevel {
                mountpoint: "/".into(),
            })
            .unwrap();
        // Response must include the mount path.
        assert!(response.data.is_some());
        let calls = helper.runner.calls.borrow();
        // Sequence: UUID query, mountpoint check, SOURCE query, mount.
        let mount_call = calls.iter().find(|(prog, _)| prog == "mount").unwrap();
        assert!(mount_call.1.contains(&"subvolid=5".to_string()));
        assert!(mount_call.1.contains(&"/dev/mapper/cryptroot".to_string()));
        assert!(
            !mount_call
                .1
                .contains(&"/dev/mapper/cryptroot[/@]".to_string())
        );
        assert!(!mount_call.1.contains(&"ro,subvolid=5".to_string()));
        drop(calls);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn strips_subvolume_suffix_from_findmnt_source_before_mounting_top_level() {
        assert_eq!(
            normalize_findmnt_source("/dev/mapper/cryptroot[/@]"),
            "/dev/mapper/cryptroot"
        );
        assert_eq!(
            normalize_findmnt_source("/dev/nvme0n1p2[/@snapshots]"),
            "/dev/nvme0n1p2"
        );
    }

    #[test]
    fn parses_mounted_and_default_subvolume_details() {
        assert_eq!(
            mounted_subvolume_from_options("rw,noatime,subvol=/@home"),
            Some(PathBuf::from("@home"))
        );
        assert_eq!(
            parse_default_subvolume_id("ID 256 gen 12 top level 5 path @"),
            Some(SubvolumeId(256))
        );
    }

    #[test]
    fn accepts_runtime_btrfs_manager_mount_targets() {
        let roots = vec![PathBuf::from("/run/user/1000/btrfs-manager")];
        validate_managed_mount_target_with_roots(
            Path::new("/run/user/1000/btrfs-manager/browse/one"),
            &roots,
        )
        .unwrap();
        let err = validate_managed_mount_target_with_roots(
            Path::new("/run/user/1000/not-managed/one"),
            &roots,
        )
        .unwrap_err();
        assert!(matches!(err, HelperError::UnsafePath(_)));
    }

    #[test]
    fn cleanup_managed_mounts_only_unmounts_managed_targets() {
        // RecordingRunner returns a /tmp/btrfs-manager-browse path for TARGET queries.
        // With the new architecture, managed roots are per-uid /run/user/<uid>/btrfs-manager.
        // Without caller_uid, managed_mount_roots is empty and nothing is unmounted.
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        let response = helper.handle(HelperRequest::CleanupManagedMounts).unwrap();
        assert!(response.ok);
        // No umount because no managed mount roots are configured without caller_uid.
        let calls = helper.runner.calls.borrow();
        assert!(!calls.iter().any(|(program, _)| program == "umount"));
    }

    #[test]
    fn rejects_path_traversal_before_command_execution() {
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        let err = helper
            .handle(HelperRequest::DeleteSnapshot {
                path: "/mnt/../bad".into(),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::UnsafePath(_)));
        assert!(helper.runner.calls.borrow().is_empty());
    }

    #[test]
    fn retention_preview_deletes_only_managed_non_anchor_snapshots() {
        let policy = SnapshotPolicy {
            id: Uuid::new_v4(),
            filesystem_id: None,
            subvolume_id: SubvolumeId(256),
            source_path: PathBuf::from("/mnt/@home"),
            mountpoint: PathBuf::from("/mnt"),
            snapshot_root: PathBuf::from(".snapshots"),
            schedule: btrfs_manager_core::PolicySchedule::Hourly,
            keep_hourly: 1,
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 0,
            enabled: true,
        };
        let now = Utc::now();
        let snapshots = vec![
            Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: PathBuf::from("/mnt/.snapshots/keep"),
                created_at: now,
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            },
            Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: PathBuf::from("/mnt/.snapshots/delete"),
                created_at: now - chrono::Duration::hours(2),
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            },
            Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: PathBuf::from("/mnt/.snapshots/anchor"),
                created_at: now - chrono::Duration::hours(3),
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::RollbackAnchor,
            },
        ];
        let preview = retention_preview_for_policy(&policy, &snapshots);
        assert_eq!(preview.keep.len(), 2);
        assert_eq!(preview.delete.len(), 1);
        assert_eq!(
            preview.delete[0].path,
            PathBuf::from("/mnt/.snapshots/delete")
        );
    }

    // Serialize tests that touch BTRFS_MANAGER_STATE_DB (process-global env var).
    static DB_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn with_test_db<T>(f: impl FnOnce() -> T) -> T {
        let _g = DB_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let db_path =
            std::env::temp_dir().join(format!("btrfs-manager-test-{}.db", Uuid::new_v4()));
        // SAFETY: DB_LOCK serializes all callers; no other thread reads this var concurrently.
        unsafe {
            std::env::set_var("BTRFS_MANAGER_STATE_DB", &db_path);
        }
        let result = f();
        unsafe {
            std::env::remove_var("BTRFS_MANAGER_STATE_DB");
        }
        std::fs::remove_file(&db_path).ok();
        result
    }

    fn find_snap(store: &StateStore, id: Uuid) -> Snapshot {
        store
            .list_all_managed_snapshots()
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
    }

    #[test]
    fn sqlite_persists_unlock_and_lock_state() {
        with_test_db(|| {
            let id = Uuid::new_v4();
            let snap = Snapshot {
                id,
                source_subvolume: SubvolumeId(256),
                path: PathBuf::from("@snapshots/managed-home-2024-01-01_00-00-00"),
                created_at: Utc::now(),
                tags: vec![],
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            };
            let store = StateStore::open().unwrap();
            store.insert_managed_snapshot(None, &snap).unwrap();

            store
                .update_snapshot_state(id, &SnapshotState::Unlocked)
                .unwrap();
            let found = find_snap(&store, id);
            assert!(
                matches!(found.state, SnapshotState::Unlocked),
                "should be Unlocked after unlock"
            );
            assert!(!matches!(
                found.state,
                SnapshotState::ReadOnly | SnapshotState::RollbackAnchor
            ));

            store
                .update_snapshot_state(id, &SnapshotState::ReadOnly)
                .unwrap();
            let found = find_snap(&store, id);
            assert!(
                matches!(found.state, SnapshotState::ReadOnly),
                "should be ReadOnly after lock"
            );
        });
    }

    #[test]
    fn set_managed_readonly_rejects_path_not_in_db() {
        with_test_db(|| {
            let tmp = std::env::temp_dir()
                .join(format!("btrfs-manager-readonly-state-{}", Uuid::new_v4()));
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &tmp);
            }
            let runner = RecordingRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let err = helper
                .handle(HelperRequest::SetManagedSnapshotReadOnly {
                    mountpoint: PathBuf::from("/mnt"),
                    subvol_path: PathBuf::from("@snapshots/external-tool-snap"),
                    readonly: false,
                })
                .unwrap_err();
            // Path not registered → rejected before any btrfs command.
            assert!(
                matches!(err, HelperError::InvalidPolicy(_)),
                "expected InvalidPolicy, got {err}"
            );
            assert!(
                !helper
                    .runner
                    .calls
                    .borrow()
                    .iter()
                    .any(|(p, a)| { p == "btrfs" && a.contains(&"property".to_string()) }),
                "btrfs property set must not be called for unregistered paths"
            );
            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(tmp).ok();
        });
    }

    struct RollbackRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for RollbackRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
                Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
                Ok("mounted\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "OPTIONS") {
                Ok("rw,relatime,subvol=/@\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "SOURCE") {
                Ok("/dev/loop-test\n".into())
            } else if program == "btrfs"
                && args.first().map(String::as_str) == Some("subvolume")
                && args.get(1).map(String::as_str) == Some("snapshot")
            {
                let source = PathBuf::from(args.get(2).expect("snapshot source"));
                let destination = PathBuf::from(args.get(3).expect("snapshot destination"));
                if !source.exists() {
                    return Err(HelperError::InvalidPolicy(format!(
                        "missing snapshot source {}",
                        source.display()
                    )));
                }
                std::fs::create_dir_all(destination)?;
                Ok("snapshot created\n".into())
            } else {
                Ok("ok\n".into())
            }
        }
    }

    struct RetentionRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for RetentionRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
                Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
                Ok("mounted\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "SOURCE") {
                Ok("/dev/loop-test\n".into())
            } else if program == "btrfs"
                && args.first().map(String::as_str) == Some("subvolume")
                && args.get(1).map(String::as_str) == Some("create")
            {
                let path = PathBuf::from(args.get(2).expect("subvolume create path"));
                std::fs::create_dir_all(path)?;
                Ok("Create subvolume\n".into())
            } else if program == "btrfs"
                && args.first().map(String::as_str) == Some("subvolume")
                && args.get(1).map(String::as_str) == Some("show")
            {
                let path = PathBuf::from(args.get(2).expect("subvolume show path"));
                if path.exists() {
                    Ok("Name: @btrfs-manager\n".into())
                } else {
                    Err(HelperError::InvalidPolicy(format!(
                        "missing subvolume {}",
                        path.display()
                    )))
                }
            } else if program == "btrfs"
                && args.first().map(String::as_str) == Some("subvolume")
                && args.get(1).map(String::as_str) == Some("snapshot")
            {
                let source = PathBuf::from(args.get(3).expect("snapshot source"));
                let destination = PathBuf::from(args.get(4).expect("snapshot destination"));
                if !source.exists() {
                    return Err(HelperError::InvalidPolicy(format!(
                        "missing snapshot source {}",
                        source.display()
                    )));
                }
                std::fs::create_dir_all(destination)?;
                Ok("snapshot created\n".into())
            } else if program == "btrfs"
                && args.first().map(String::as_str) == Some("subvolume")
                && args.get(1).map(String::as_str) == Some("delete")
            {
                let path = PathBuf::from(args.get(2).expect("subvolume delete path"));
                std::fs::remove_dir_all(path)?;
                Ok("deleted\n".into())
            } else {
                Ok("ok\n".into())
            }
        }
    }

    struct DiagnosticsRunner;

    impl CommandRunner for DiagnosticsRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
            if program == "btrfs" && args.first().map(String::as_str) == Some("version") {
                Ok("btrfs-progs v6.16\n".into())
            } else if program == "btrfs"
                && args.first().map(String::as_str) == Some("subvolume")
                && args.get(1).map(String::as_str) == Some("show")
            {
                Ok("Name: @btrfs-manager\n".into())
            } else if program == "findmnt"
                && args.iter().any(|arg| arg == "FSTYPE,SOURCE,TARGET,OPTIONS")
            {
                Ok("FSTYPE=\"btrfs\" SOURCE=\"/dev/loop0[/@]\" TARGET=\"/\" OPTIONS=\"rw,relatime,subvol=/@\"\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
                Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
            } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
                Ok("mounted\n".into())
            } else if program == "systemctl"
                && args.first().map(String::as_str) == Some("is-active")
            {
                Ok("active\n".into())
            } else {
                Ok("ok\n".into())
            }
        }
    }

    #[test]
    fn diagnostics_report_contains_core_checks() {
        with_test_db(|| {
            let test_root =
                std::env::temp_dir().join(format!("btrfs-manager-diagnostics-{}", Uuid::new_v4()));
            let top = test_root.join("550e8400-e29b-41d4-a716-446655440000");
            std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
            }

            let helper = Helper::new(DiagnosticsRunner);
            let response = helper.handle(HelperRequest::RunDiagnostics).unwrap();
            let report: DiagnosticsReport = serde_json::from_value(response.data.unwrap()).unwrap();
            let names = report
                .checks
                .iter()
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>();
            assert!(names.contains(&"Helper"));
            assert!(names.contains(&"Btrfs tools"));
            assert!(names.contains(&"Root filesystem"));
            assert!(names.contains(&"Btrfs top-level mount"));
            assert!(names.contains(&"Rollback"));
            assert!(report.checks.iter().any(|check| {
                check.name == "Root filesystem" && matches!(check.status, DiagnosticStatus::Ok)
            }));

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    struct FailingSystemdRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for FailingSystemdRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            if program == "systemctl" && args.iter().any(|arg| arg == "enable" || arg == "disable")
            {
                return Err(HelperError::CommandFailed {
                    program: program.into(),
                    args: args.to_vec(),
                    stderr: "systemd failed".into(),
                });
            }
            RetentionRunner {
                calls: RefCell::new(Vec::new()),
            }
            .run(program, args)
        }
    }

    fn test_policy(id: Uuid) -> SnapshotPolicy {
        SnapshotPolicy {
            id,
            filesystem_id: None,
            subvolume_id: SubvolumeId(256),
            source_path: PathBuf::from("@"),
            mountpoint: PathBuf::from("/mnt"),
            snapshot_root: PathBuf::from("@btrfs-manager/scheduled"),
            schedule: btrfs_manager_core::PolicySchedule::Hourly,
            keep_hourly: 1,
            keep_daily: 0,
            keep_weekly: 0,
            keep_monthly: 0,
            enabled: true,
        }
    }

    #[test]
    fn upsert_policy_writes_systemd_timer_and_enable_command() {
        with_test_db(|| {
            let test_root =
                std::env::temp_dir().join(format!("btrfs-manager-scheduler-{}", Uuid::new_v4()));
            let systemd_dir = test_root.join("systemd");
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
                std::env::set_var("BTRFS_MANAGER_SYSTEMD_DIR", &systemd_dir);
            }

            let policy = SnapshotPolicy {
                schedule: btrfs_manager_core::PolicySchedule::Daily,
                ..test_policy(Uuid::new_v4())
            };
            let runner = RetentionRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            helper
                .handle(HelperRequest::UpsertSnapshotPolicy {
                    policy: policy.clone(),
                })
                .unwrap();

            let service = std::fs::read_to_string(
                systemd_dir.join(format!("btrfs-manager-policy-{}.service", policy.id)),
            )
            .unwrap();
            let timer = std::fs::read_to_string(
                systemd_dir.join(format!("btrfs-manager-policy-{}.timer", policy.id)),
            )
            .unwrap();
            assert!(service.contains(&format!("run-retention-policy --policy-id {}", policy.id)));
            assert!(timer.contains("OnCalendar=daily"));
            assert!(timer.contains("Persistent=true"));

            let calls = helper.runner.calls.borrow();
            assert!(calls.iter().any(|(program, args)| {
                program == "systemctl" && args == &["daemon-reload".to_string()]
            }));
            assert!(calls.iter().any(|(program, args)| {
                program == "systemctl"
                    && args
                        == &[
                            "enable".to_string(),
                            "--now".to_string(),
                            format!("btrfs-manager-policy-{}.timer", policy.id),
                        ]
            }));

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
                std::env::remove_var("BTRFS_MANAGER_SYSTEMD_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn upsert_policy_rolls_back_db_when_timer_enable_fails() {
        with_test_db(|| {
            let test_root = std::env::temp_dir().join(format!(
                "btrfs-manager-scheduler-failure-{}",
                Uuid::new_v4()
            ));
            let systemd_dir = test_root.join("systemd");
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
                std::env::set_var("BTRFS_MANAGER_SYSTEMD_DIR", &systemd_dir);
            }

            let policy = test_policy(Uuid::new_v4());
            let runner = FailingSystemdRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let err = helper
                .handle(HelperRequest::UpsertSnapshotPolicy {
                    policy: policy.clone(),
                })
                .unwrap_err();
            assert!(matches!(err, HelperError::CommandFailed { .. }));

            let top = test_root.join("550e8400-e29b-41d4-a716-446655440000");
            let store = StateStore::open_at(top.join("@btrfs-manager/state/state.db")).unwrap();
            assert!(
                store.get_policy(policy.id).unwrap().is_none(),
                "policy must not remain in DB when timer activation fails"
            );
            assert!(
                !systemd_dir
                    .join(format!("btrfs-manager-policy-{}.service", policy.id))
                    .exists(),
                "service unit should be removed when a new policy fails to activate"
            );
            assert!(
                !systemd_dir
                    .join(format!("btrfs-manager-policy-{}.timer", policy.id))
                    .exists(),
                "timer unit should be removed when a new policy fails to activate"
            );

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
                std::env::remove_var("BTRFS_MANAGER_SYSTEMD_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn scheduled_retention_run_creates_snapshot_deletes_expired_and_logs_result() {
        with_test_db(|| {
            let test_root =
                std::env::temp_dir().join(format!("btrfs-manager-retention-{}", Uuid::new_v4()));
            let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
            let top = test_root.join(fs_uuid);
            std::fs::create_dir_all(top.join("@/etc")).unwrap();
            std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
            }

            let policy = SnapshotPolicy {
                keep_hourly: 2,
                ..test_policy(Uuid::new_v4())
            };
            let state_db = top.join("@btrfs-manager/state/state.db");
            let store = StateStore::open_at(state_db.clone()).unwrap();
            store.upsert_policy(&policy).unwrap();

            let old_bucket = Utc.with_ymd_and_hms(2026, 1, 1, 10, 30, 0).unwrap();
            let keep = Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: policy_snapshot_dir(&policy).join("keep-current-hour"),
                created_at: old_bucket,
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            };
            let expired = Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: policy_snapshot_dir(&policy).join("delete-older-hour"),
                created_at: old_bucket - chrono::Duration::minutes(10),
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            };
            let anchor = Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: policy_snapshot_dir(&policy).join("keep-anchor"),
                created_at: old_bucket - chrono::Duration::hours(3),
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::RollbackAnchor,
            };
            for snapshot in [&keep, &expired, &anchor] {
                std::fs::create_dir_all(top.join(&snapshot.path)).unwrap();
                store
                    .insert_managed_snapshot(Some(policy.id), snapshot)
                    .unwrap();
            }

            let runner = RetentionRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let response = helper
                .handle(HelperRequest::RunRetentionPolicy {
                    policy_id: policy.id,
                })
                .unwrap();
            let log: PolicyRunLog = serde_json::from_value(response.data.unwrap()).unwrap();
            assert!(matches!(log.status, PolicyRunStatus::Success));
            let created = log.created_snapshot.expect("run should create a snapshot");
            assert_eq!(log.deleted_snapshots, vec![expired.path.clone()]);

            assert!(top.join(&created).exists(), "new snapshot should exist");
            assert!(
                top.join(&keep.path).exists(),
                "newest retained snapshot should remain"
            );
            assert!(
                !top.join(&expired.path).exists(),
                "expired snapshot should be deleted"
            );
            assert!(
                top.join(&anchor.path).exists(),
                "rollback anchor must never be deleted by retention"
            );

            let after = StateStore::open_at(state_db).unwrap();
            let paths: Vec<_> = after
                .list_managed_snapshots_for_policy(policy.id)
                .unwrap()
                .into_iter()
                .map(|snapshot| snapshot.path)
                .collect();
            assert!(paths.contains(&created));
            assert!(paths.contains(&keep.path));
            assert!(paths.contains(&anchor.path));
            assert!(!paths.contains(&expired.path));

            let logs = after.list_policy_logs(policy.id).unwrap();
            assert_eq!(logs.len(), 1);
            assert!(matches!(logs[0].status, PolicyRunStatus::Success));
            assert_eq!(logs[0].deleted_snapshots, vec![expired.path]);

            let calls = helper.runner.calls.borrow();
            assert!(calls.iter().any(|(program, args)| {
                program == "btrfs"
                    && args.first().map(String::as_str) == Some("subvolume")
                    && args.get(1).map(String::as_str) == Some("snapshot")
            }));
            assert!(calls.iter().any(|(program, args)| {
                program == "btrfs"
                    && args.first().map(String::as_str) == Some("subvolume")
                    && args.get(1).map(String::as_str) == Some("delete")
                    && args
                        .get(2)
                        .is_some_and(|path| path.ends_with("delete-older-hour"))
            }));

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn scheduled_retention_run_removes_stale_db_entries_for_missing_snapshots() {
        with_test_db(|| {
            let test_root = std::env::temp_dir()
                .join(format!("btrfs-manager-retention-stale-{}", Uuid::new_v4()));
            let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
            let top = test_root.join(fs_uuid);
            std::fs::create_dir_all(top.join("@/etc")).unwrap();
            std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
            }

            let policy = SnapshotPolicy {
                keep_hourly: 1,
                ..test_policy(Uuid::new_v4())
            };
            let state_db = top.join("@btrfs-manager/state/state.db");
            let store = StateStore::open_at(state_db.clone()).unwrap();
            store.upsert_policy(&policy).unwrap();

            let now = Utc.with_ymd_and_hms(2026, 1, 1, 10, 30, 0).unwrap();
            let keep = Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: policy_snapshot_dir(&policy).join("keep-current-hour"),
                created_at: now,
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            };
            let stale = Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: policy_snapshot_dir(&policy).join("already-missing"),
                created_at: now - chrono::Duration::hours(2),
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            };
            std::fs::create_dir_all(top.join(&keep.path)).unwrap();
            store
                .insert_managed_snapshot(Some(policy.id), &keep)
                .unwrap();
            store
                .insert_managed_snapshot(Some(policy.id), &stale)
                .unwrap();

            let runner = RetentionRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let response = helper
                .handle(HelperRequest::RunRetentionPolicy {
                    policy_id: policy.id,
                })
                .unwrap();
            let log: PolicyRunLog = serde_json::from_value(response.data.unwrap()).unwrap();
            assert!(matches!(log.status, PolicyRunStatus::Success));
            assert!(
                !log.deleted_snapshots.contains(&stale.path),
                "missing subvolume was not deleted during this run"
            );

            let after = StateStore::open_at(state_db).unwrap();
            let paths: Vec<_> = after
                .list_managed_snapshots_for_policy(policy.id)
                .unwrap()
                .into_iter()
                .map(|snapshot| snapshot.path)
                .collect();
            assert!(paths.contains(&keep.path));
            assert!(!paths.contains(&stale.path));

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn scheduled_retention_run_removes_created_snapshot_when_db_insert_fails() {
        with_test_db(|| {
            let test_root = std::env::temp_dir().join(format!(
                "btrfs-manager-retention-insert-failure-{}",
                Uuid::new_v4()
            ));
            let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
            let top = test_root.join(fs_uuid);
            std::fs::create_dir_all(top.join("@/etc")).unwrap();
            std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
            }

            let policy = test_policy(Uuid::new_v4());
            let state_db = top.join("@btrfs-manager/state/state.db");
            let store = StateStore::open_at(state_db.clone()).unwrap();
            store.upsert_policy(&policy).unwrap();
            drop(store);
            let conn = rusqlite::Connection::open(&state_db).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER fail_managed_snapshot_insert
                 BEFORE INSERT ON managed_snapshots
                 BEGIN
                   SELECT RAISE(FAIL, 'injected managed snapshot insert failure');
                 END;",
            )
            .unwrap();
            drop(conn);

            let runner = RetentionRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let response = helper
                .handle(HelperRequest::RunRetentionPolicy {
                    policy_id: policy.id,
                })
                .unwrap();
            assert!(!response.ok);
            let log: PolicyRunLog = serde_json::from_value(response.data.unwrap()).unwrap();
            assert!(matches!(log.status, PolicyRunStatus::Failed));
            assert_eq!(
                log.created_snapshot, None,
                "created snapshot should be cleared after successful cleanup"
            );
            assert!(
                log.error
                    .as_deref()
                    .is_some_and(|error| error.contains("removed the created snapshot")),
                "failure log should describe recovery cleanup"
            );
            let snapshot_dir = top.join(policy_snapshot_dir(&policy));
            let remaining_entries = if snapshot_dir.exists() {
                std::fs::read_dir(&snapshot_dir).unwrap().count()
            } else {
                0
            };
            assert_eq!(
                remaining_entries, 0,
                "failed DB insert must not leave an untracked snapshot directory"
            );
            let calls = helper.runner.calls.borrow();
            assert!(calls.iter().any(|(program, args)| {
                program == "btrfs"
                    && args.first().map(String::as_str) == Some("subvolume")
                    && args.get(1).map(String::as_str) == Some("delete")
                    && args
                        .get(2)
                        .is_some_and(|path| path.contains(&policy.id.to_string()))
            }));

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn rollback_rejects_absolute_or_traversing_paths_before_commands() {
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        let err = helper
            .handle(HelperRequest::StageRollback {
                mountpoint: PathBuf::from("/mnt"),
                snapshot_path: PathBuf::from("/etc"),
                return_snapshot_path: PathBuf::from("@btrfs-manager/return"),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(helper.runner.calls.borrow().is_empty());

        let err = helper
            .handle(HelperRequest::StageRollback {
                mountpoint: PathBuf::from("/mnt"),
                snapshot_path: PathBuf::from("@snapshots/one"),
                return_snapshot_path: PathBuf::from("../escape"),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(helper.runner.calls.borrow().is_empty());
    }

    #[test]
    fn rollback_stage_and_revert_preserve_return_anchor() {
        with_test_db(|| {
            unsafe {
                std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-before-rollback");
            }
            let test_root =
                std::env::temp_dir().join(format!("btrfs-manager-rollback-{}", Uuid::new_v4()));
            let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
            let top = test_root.join(fs_uuid);
            let active_root = top.join("@");
            let source_snapshot = top.join("@btrfs-manager/managed-root-snap");
            std::fs::create_dir_all(active_root.join("etc")).unwrap();
            std::fs::write(active_root.join("etc/original.conf"), "original\n").unwrap();
            std::fs::create_dir_all(&source_snapshot).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
            }

            let runner = RollbackRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let response = helper
                .handle(HelperRequest::StageRollback {
                    mountpoint: PathBuf::from("/mnt"),
                    snapshot_path: PathBuf::from("@btrfs-manager/managed-root-snap"),
                    return_snapshot_path: PathBuf::from("@btrfs-manager/return-root"),
                })
                .unwrap();
            let plan: RollbackPlan = serde_json::from_value(response.data.unwrap()).unwrap();
            let plan_file = top
                .join("@btrfs-manager")
                .join("rollback-plans")
                .join(format!("{}.json", plan.id));
            let state_db = top.join("@btrfs-manager").join("state").join("state.db");

            assert!(top.join("@").exists(), "staged root should exist");
            assert!(
                top.join("@btrfs-manager/return-root/etc/original.conf")
                    .exists(),
                "current root should be preserved as return anchor"
            );
            assert!(
                plan_file.exists(),
                "rollback plan should be persisted outside the restored root"
            );
            assert!(
                state_db.exists(),
                "state database should be persisted outside the restored root"
            );

            let store = StateStore::open_at(state_db.clone()).unwrap();
            let anchor = store
                .list_all_managed_snapshots()
                .unwrap()
                .into_iter()
                .find(|snapshot| snapshot.path == Path::new("@btrfs-manager/return-root"))
                .expect("rollback anchor should be stored");
            assert!(matches!(anchor.state, SnapshotState::RollbackAnchor));
            assert!(
                anchor
                    .tags
                    .iter()
                    .any(|tag| tag == "before-restoring:@btrfs-manager/managed-root-snap")
            );
            let pending = store.get_pending_rollback().unwrap().unwrap();
            assert_eq!(
                pending.created_boot_id.as_deref(),
                Some("boot-before-rollback")
            );
            assert_eq!(
                pending.description.as_deref(),
                Some("Before restoring @btrfs-manager/managed-root-snap")
            );

            unsafe {
                std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-after-rollback");
            }
            let pending_response = helper.handle(HelperRequest::GetPendingRollback).unwrap();
            let prompt: RollbackPrompt =
                serde_json::from_value(pending_response.data.unwrap()).unwrap();
            assert!(prompt.rebooted_since_staging);

            unsafe {
                std::env::set_var(
                    "BTRFS_MANAGER_STATE_DB",
                    test_root.join("restored-root-empty-state.db"),
                );
            }
            let fallback_response = helper.handle(HelperRequest::GetPendingRollback).unwrap();
            let fallback_prompt: RollbackPrompt =
                serde_json::from_value(fallback_response.data.unwrap()).unwrap();
            assert_eq!(fallback_prompt.plan.id, plan.id);
            assert!(fallback_prompt.rebooted_since_staging);

            helper
                .handle(HelperRequest::RevertRollback { plan_id: plan.id })
                .unwrap();
            assert!(
                top.join("@/etc/original.conf").exists(),
                "revert should restore the return anchor to the active root path"
            );
            let file_plan: RollbackPlan =
                serde_json::from_slice(&std::fs::read(&plan_file).unwrap()).unwrap();
            assert!(matches!(file_plan.status, RollbackStatus::Reverted));
            assert!(store.get_pending_rollback().unwrap().is_none());
            let no_pending_after_revert = helper.handle(HelperRequest::GetPendingRollback).unwrap();
            assert!(no_pending_after_revert.data.is_none());

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_BOOT_ID");
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn rollback_commit_resolves_top_level_plan_even_if_db_would_still_be_pending() {
        with_test_db(|| {
            unsafe {
                std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-before-rollback");
            }
            let test_root =
                std::env::temp_dir().join(format!("btrfs-manager-commit-{}", Uuid::new_v4()));
            let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
            let top = test_root.join(fs_uuid);
            let active_root = top.join("@");
            let source_snapshot = top.join("@btrfs-manager/managed-root-snap");
            std::fs::create_dir_all(active_root.join("etc")).unwrap();
            std::fs::write(active_root.join("etc/original.conf"), "original\n").unwrap();
            std::fs::create_dir_all(&source_snapshot).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
            }

            let runner = RollbackRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            let response = helper
                .handle(HelperRequest::StageRollback {
                    mountpoint: PathBuf::from("/mnt"),
                    snapshot_path: PathBuf::from("@btrfs-manager/managed-root-snap"),
                    return_snapshot_path: PathBuf::from("@btrfs-manager/return-root"),
                })
                .unwrap();
            let plan: RollbackPlan = serde_json::from_value(response.data.unwrap()).unwrap();
            let plan_file = top
                .join("@btrfs-manager")
                .join("rollback-plans")
                .join(format!("{}.json", plan.id));
            let state_db = top.join("@btrfs-manager").join("state").join("state.db");
            let store = StateStore::open_at(state_db).unwrap();
            assert!(
                store.get_pending_rollback().unwrap().is_some(),
                "DB should still have an awaiting rollback before commit"
            );

            unsafe {
                std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-after-rollback");
            }
            helper
                .handle(HelperRequest::CommitRollback { plan_id: plan.id })
                .unwrap();

            let file_plan: RollbackPlan =
                serde_json::from_slice(&std::fs::read(&plan_file).unwrap()).unwrap();
            assert!(matches!(file_plan.status, RollbackStatus::Activated));
            assert!(store.get_pending_rollback().unwrap().is_none());
            let no_pending_after_commit = helper.handle(HelperRequest::GetPendingRollback).unwrap();
            assert!(
                no_pending_after_commit.data.is_none(),
                "resolved top-level plan should suppress stale rollback prompts"
            );

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_BOOT_ID");
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }

    #[test]
    fn rollback_creates_manager_subvolume_before_persisting_state() {
        with_test_db(|| {
            let test_root =
                std::env::temp_dir().join(format!("btrfs-manager-new-state-{}", Uuid::new_v4()));
            let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
            let top = test_root.join(fs_uuid);
            let active_root = top.join("@");
            let source_snapshot = top.join("@snapshots/managed-root-snap");
            std::fs::create_dir_all(active_root.join("etc")).unwrap();
            std::fs::write(active_root.join("etc/original.conf"), "original\n").unwrap();
            std::fs::create_dir_all(&source_snapshot).unwrap();
            unsafe {
                std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
                std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-before-rollback");
            }

            let runner = RollbackRunner {
                calls: RefCell::new(Vec::new()),
            };
            let helper = Helper::new(runner);
            helper
                .handle(HelperRequest::StageRollback {
                    mountpoint: PathBuf::from("/mnt"),
                    snapshot_path: PathBuf::from("@snapshots/managed-root-snap"),
                    return_snapshot_path: PathBuf::from("@btrfs-manager/return-root"),
                })
                .unwrap();

            assert!(
                helper.runner.calls.borrow().iter().any(|(program, args)| {
                    program == "btrfs"
                        && args.first().map(String::as_str) == Some("subvolume")
                        && args.get(1).map(String::as_str) == Some("create")
                        && args
                            .get(2)
                            .is_some_and(|path| path.ends_with("@btrfs-manager"))
                }),
                "@btrfs-manager must be created as a Btrfs subvolume, not a plain directory"
            );
            assert!(
                top.join("@btrfs-manager/state/state.db").exists(),
                "state DB should live under the manager subvolume path"
            );

            unsafe {
                std::env::remove_var("BTRFS_MANAGER_BOOT_ID");
                std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
            }
            std::fs::remove_dir_all(test_root).ok();
        });
    }
}
