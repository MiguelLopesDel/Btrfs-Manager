use btrfs_manager_core::models::{
    BootIntegration, FilesystemId, FilesystemMount, FilesystemSummary, Snapshot, SnapshotOrigin,
    SnapshotPolicy, SnapshotState, Subvolume, SubvolumeId, SubvolumeKind,
};
use btrfs_manager_core::parser::{ParseError, parse_btrfs_subvolume_list, parse_findmnt_pairs};
use btrfs_manager_core::paths::{PathSafetyError, validate_absolute_no_traversal};
use btrfs_manager_core::retention::{RetentionPolicy, retention_keep_set};
use btrfs_manager_core::{PolicyRunLog, PolicyRunStatus, RetentionPreview};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;
use uuid::Uuid;

pub mod dbus;

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
    MountTopLevel {
        mountpoint: PathBuf,
        target: PathBuf,
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
    DeleteManagedSnapshot {
        path: PathBuf,
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
    StageRollback {
        snapshot: PathBuf,
        prepared_subvolume: PathBuf,
        return_snapshot: PathBuf,
    },
    RunRetentionPolicy {
        policy_id: Uuid,
    },
    ListPolicyRunLogs {
        policy_id: Uuid,
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
        let mut roots = vec![
            std::env::temp_dir().join("btrfs-manager-browse"),
            std::env::temp_dir().join("btrfs-manager-toplevel"),
        ];
        if let Some(runtime_dir) = runtime_dir_from_env() {
            roots.push(runtime_dir.join("btrfs-manager"));
        }
        if let Some(uid) = self.caller_uid {
            let caller_runtime = PathBuf::from("/run/user").join(uid.to_string());
            let caller_dir = caller_runtime.join("btrfs-manager");
            if !roots.contains(&caller_dir) {
                roots.push(caller_dir);
            }
        }
        roots
    }

    pub fn handle(&self, request: HelperRequest) -> Result<HelperResponse, HelperError> {
        match request {
            HelperRequest::DiscoverFilesystems => self.discover_filesystems(),
            HelperRequest::ListSubvolumes { mountpoint } => {
                validate_path(&mountpoint)?;
                let args = vec![
                    "subvolume".into(),
                    "list".into(),
                    "-u".into(),
                    mountpoint.display().to_string(),
                ];
                let output = self.runner.run("btrfs", &args)?;
                let mut subvolumes = parse_btrfs_subvolume_list(&output)?;
                classify_subvolumes(&mut subvolumes);
                // Mark subvolumes that have a managed snapshot record in SQLite.
                if let Ok(managed) = StateStore::open().and_then(|s| s.list_managed_snapshot_paths()) {
                    for subvolume in &mut subvolumes {
                        if managed.contains(&subvolume.path) {
                            subvolume.managed = true;
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
            HelperRequest::MountTopLevel { mountpoint, target } => {
                validate_path(&mountpoint)?;
                validate_path(&target)?;
                tracing::debug!(
                    mountpoint = %mountpoint.display(),
                    target = %target.display(),
                    "mounting btrfs top-level"
                );
                let mountpoint_arg = mountpoint.display().to_string();
                let findmnt_args = vec![
                    "-n".into(),
                    "-o".into(),
                    "SOURCE".into(),
                    "--target".into(),
                    mountpoint_arg,
                ];
                let source_output = self.runner.run("findmnt", &findmnt_args)?;
                let source = normalize_findmnt_source(source_output.trim());
                tracing::debug!(device = %source, "resolved block device");
                let mount_args = vec![
                    "-o".into(),
                    "ro,subvolid=5".into(),
                    source,
                    target.display().to_string(),
                ];
                self.runner.run("mount", &mount_args)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "top-level mounted".into(),
                    data: None,
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
            } => {
                validate_path(&mountpoint)?;
                // Resolve the block device for this mountpoint
                let findmnt_args = vec![
                    "-n".into(), "-o".into(), "SOURCE".into(),
                    "--target".into(), mountpoint.display().to_string(),
                ];
                let device_raw = self.runner.run("findmnt", &findmnt_args)?;
                let device = normalize_findmnt_source(device_raw.trim());

                // Mount the Btrfs top-level (subvolid=5) writable to a unique temp dir.
                // Using a dedicated temp dir keeps this mount invisible to browse cleanup.
                let temp_suffix = Uuid::new_v4().simple().to_string();
                let top = std::env::temp_dir()
                    .join(format!("btrfs-manager-snap-{temp_suffix}"));
                std::fs::create_dir_all(&top)?;

                let snapshot_result: Result<Snapshot, HelperError> = (|| {
                    let mount_args = vec![
                        "-o".into(), "subvolid=5".into(),
                        device, top.display().to_string(),
                    ];
                    self.runner.run("mount", &mount_args)?;

                    let source = top.join(&subvolume_path);
                    let timestamp = Utc::now().format("%Y-%m-%d_%H-%M-%S");
                    let dest_name = format!("managed-{timestamp}");
                    let dest_parent = top.join(&snapshot_root);
                    std::fs::create_dir_all(&dest_parent)?;
                    let dest = dest_parent.join(&dest_name);

                    let snap_args = vec![
                        "subvolume".into(), "snapshot".into(), "-r".into(),
                        source.display().to_string(), dest.display().to_string(),
                    ];
                    self.runner.run("btrfs", &snap_args)?;

                    // Path stored in SQLite is relative to the Btrfs volume root
                    let rel_path = snapshot_root.join(dest_name);
                    Ok(Snapshot {
                        id: Uuid::new_v4(),
                        source_subvolume: SubvolumeId(0),
                        path: rel_path,
                        created_at: Utc::now(),
                        tags,
                        origin: SnapshotOrigin::Managed,
                        state: SnapshotState::ReadOnly,
                    })
                })();

                // Always unmount the temp mount regardless of snapshot result.
                let _ = self.runner.run("umount", &[top.display().to_string()]);
                let _ = std::fs::remove_dir(&top);

                let snapshot = snapshot_result?;
                StateStore::open()?.insert_managed_snapshot(None, &snapshot)?;
                tracing::info!(path = %snapshot.path.display(), "managed snapshot created");
                Ok(HelperResponse {
                    ok: true,
                    message: format!("snapshot created at {}", snapshot.path.display()),
                    data: Some(serde_json::to_value(&snapshot)?),
                })
            }
            HelperRequest::ListManagedSnapshots => {
                let snapshots = StateStore::open()?.list_all_managed_snapshots()?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("found {} managed snapshot(s)", snapshots.len()),
                    data: Some(serde_json::to_value(&snapshots)?),
                })
            }
            HelperRequest::DeleteManagedSnapshot { path } => {
                validate_path(&path)?;
                let store = StateStore::open()?;
                // Verify this path is in the managed_snapshots table before deleting.
                let id = store.find_managed_snapshot_id_by_path(&path)?;
                let args = vec!["subvolume".into(), "delete".into(), path.display().to_string()];
                self.runner.run("btrfs", &args)?;
                store.delete_managed_snapshot(id)?;
                tracing::info!(path = %path.display(), "managed snapshot deleted");
                Ok(HelperResponse {
                    ok: true,
                    message: format!("snapshot deleted: {}", path.display()),
                    data: None,
                })
            }
            HelperRequest::ListSnapshotPolicies => {
                let policies = StateStore::open()?.list_policies()?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("found {} snapshot policies", policies.len()),
                    data: Some(serde_json::to_value(policies)?),
                })
            }
            HelperRequest::UpsertSnapshotPolicy { policy } => {
                validate_policy(&policy)?;
                let store = StateStore::open()?;
                store.upsert_policy(&policy)?;
                self.write_systemd_policy_units(&policy)?;
                Ok(HelperResponse {
                    ok: true,
                    message: "snapshot policy saved".into(),
                    data: Some(serde_json::to_value(policy)?),
                })
            }
            HelperRequest::SetSnapshotPolicyEnabled { policy_id, enabled } => {
                let store = StateStore::open()?;
                let mut policy = store.get_policy(policy_id)?.ok_or_else(|| {
                    HelperError::InvalidPolicy(format!("unknown policy {policy_id}"))
                })?;
                policy.enabled = enabled;
                store.upsert_policy(&policy)?;
                self.write_systemd_policy_units(&policy)?;
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
                let snapshots = StateStore::open()?.list_managed_snapshots_for_policy(policy.id)?;
                let preview = retention_preview_for_policy(&policy, &snapshots);
                Ok(HelperResponse {
                    ok: true,
                    message: format!("{} snapshot(s) would be deleted", preview.delete.len()),
                    data: Some(serde_json::to_value(preview)?),
                })
            }
            HelperRequest::StageRollback { .. } => {
                Err(HelperError::NotImplemented("stage rollback"))
            }
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
                let logs = StateStore::open()?.list_policy_logs(policy_id)?;
                Ok(HelperResponse {
                    ok: true,
                    message: format!("found {} policy run log(s)", logs.len()),
                    data: Some(serde_json::to_value(logs)?),
                })
            }
        }
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
        let store = StateStore::open()?;
        let policy = store
            .get_policy(policy_id)?
            .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
        let snapshots = store.list_managed_snapshots_for_policy(policy_id)?;
        Ok(retention_preview_for_policy(&policy, &snapshots))
    }

    fn run_retention_policy(&self, policy_id: Uuid) -> Result<PolicyRunLog, HelperError> {
        let started_at = Utc::now();
        let log_id = Uuid::new_v4();
        let result = self.run_retention_policy_inner(policy_id);
        let finished_at = Utc::now();
        let log = match result {
            Ok((created_snapshot, deleted_snapshots)) => PolicyRunLog {
                id: log_id,
                policy_id,
                started_at,
                finished_at,
                status: PolicyRunStatus::Success,
                created_snapshot: Some(created_snapshot),
                deleted_snapshots,
                error: None,
            },
            Err(err) => PolicyRunLog {
                id: log_id,
                policy_id,
                started_at,
                finished_at,
                status: PolicyRunStatus::Failed,
                created_snapshot: None,
                deleted_snapshots: Vec::new(),
                error: Some(err.to_string()),
            },
        };
        StateStore::open()?.insert_policy_log(&log)?;
        Ok(log)
    }

    fn run_retention_policy_inner(
        &self,
        policy_id: Uuid,
    ) -> Result<(PathBuf, Vec<PathBuf>), HelperError> {
        let store = StateStore::open()?;
        let policy = store
            .get_policy(policy_id)?
            .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
        if !policy.enabled {
            return Err(HelperError::InvalidPolicy(format!(
                "policy {policy_id} is disabled"
            )));
        }

        validate_policy(&policy)?;
        let source = policy.source_path.clone();
        validate_path(&source)?;
        let snapshot_dir = policy_snapshot_dir(&policy);
        std::fs::create_dir_all(&snapshot_dir)?;
        let destination = snapshot_dir.join(format!(
            "{}-{}",
            sanitize_snapshot_label(&policy.source_path),
            Utc::now().format("%Y%m%d-%H%M%S")
        ));
        validate_path(&destination)?;

        let args = vec![
            "subvolume".into(),
            "snapshot".into(),
            "-r".into(),
            source.display().to_string(),
            destination.display().to_string(),
        ];
        self.runner.run("btrfs", &args)?;

        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            source_subvolume: policy.subvolume_id.clone(),
            path: destination.clone(),
            created_at: Utc::now(),
            tags: vec!["scheduled".into()],
            origin: SnapshotOrigin::Managed,
            state: SnapshotState::ReadOnly,
        };
        store.insert_managed_snapshot(Some(policy_id), &snapshot)?;

        let snapshots = store.list_managed_snapshots_for_policy(policy_id)?;
        let keep = retention_keep_set(&snapshots, &retention_policy_from_snapshot_policy(&policy));
        let mut deleted = Vec::new();
        for snapshot in snapshots {
            if keep.contains(&snapshot.id) || !snapshot.is_managed() {
                continue;
            }
            if snapshot.state == SnapshotState::RollbackAnchor {
                continue;
            }
            validate_path(&snapshot.path)?;
            let args = vec![
                "subvolume".into(),
                "delete".into(),
                snapshot.path.display().to_string(),
            ];
            self.runner.run("btrfs", &args)?;
            store.delete_managed_snapshot(snapshot.id)?;
            deleted.push(snapshot.path);
        }

        Ok((destination, deleted))
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
            let _ = self.runner.run(
                "systemctl",
                &["disable".into(), "--now".into(), timer_name.into()],
            );
        }
        let _ = service_name;
        Ok(())
    }
}

struct StateStore {
    connection: Connection,
}

impl StateStore {
    fn open() -> Result<Self, HelperError> {
        let path = state_db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), HelperError> {
        self.connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS managed_snapshots (
                id TEXT PRIMARY KEY NOT NULL,
                policy_id TEXT,
                source_subvolume_id INTEGER NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                tags_json TEXT NOT NULL DEFAULT '[]',
                origin_tool TEXT,
                state TEXT NOT NULL CHECK (state IN ('readonly', 'unlocked', 'dirty_unlocked', 'rollback_anchor'))
            );
            CREATE TABLE IF NOT EXISTS snapshot_policies (
                id TEXT PRIMARY KEY NOT NULL,
                filesystem_id TEXT,
                subvolume_id INTEGER NOT NULL,
                source_path TEXT NOT NULL,
                mountpoint TEXT NOT NULL,
                snapshot_root TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                schedule TEXT NOT NULL CHECK (schedule IN ('hourly', 'daily', 'weekly', 'monthly')),
                keep_hourly INTEGER NOT NULL DEFAULT 24,
                keep_daily INTEGER NOT NULL DEFAULT 7,
                keep_weekly INTEGER NOT NULL DEFAULT 4,
                keep_monthly INTEGER NOT NULL DEFAULT 6,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS policy_run_logs (
                id TEXT PRIMARY KEY NOT NULL,
                policy_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('success', 'failed')),
                created_snapshot TEXT,
                deleted_snapshots_json TEXT NOT NULL DEFAULT '[]',
                error TEXT
            );
            "#,
        )?;
        add_column_if_missing(&self.connection, "managed_snapshots", "policy_id", "TEXT")?;
        for (column, definition) in [
            ("filesystem_id", "TEXT"),
            ("source_path", "TEXT NOT NULL DEFAULT ''"),
            ("mountpoint", "TEXT NOT NULL DEFAULT '/'"),
            ("snapshot_root", "TEXT NOT NULL DEFAULT '.snapshots'"),
            ("created_at", "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"),
            ("updated_at", "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"),
        ] {
            add_column_if_missing(&self.connection, "snapshot_policies", column, definition)?;
        }
        Ok(())
    }

    fn list_policies(&self) -> Result<Vec<SnapshotPolicy>, HelperError> {
        let mut statement = self.connection.prepare(
            "SELECT id, filesystem_id, subvolume_id, source_path, mountpoint, snapshot_root, schedule, keep_hourly, keep_daily, keep_weekly, keep_monthly, enabled FROM snapshot_policies ORDER BY source_path",
        )?;
        let policies = statement
            .query_map([], policy_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(policies)
    }

    fn get_policy(&self, id: Uuid) -> Result<Option<SnapshotPolicy>, HelperError> {
        self.connection
            .query_row(
                "SELECT id, filesystem_id, subvolume_id, source_path, mountpoint, snapshot_root, schedule, keep_hourly, keep_daily, keep_weekly, keep_monthly, enabled FROM snapshot_policies WHERE id = ?1",
                params![id.to_string()],
                policy_from_row,
            )
            .optional()
            .map_err(HelperError::from)
    }

    fn upsert_policy(&self, policy: &SnapshotPolicy) -> Result<(), HelperError> {
        self.connection.execute(
            "INSERT INTO snapshot_policies (id, filesystem_id, subvolume_id, source_path, mountpoint, snapshot_root, schedule, keep_hourly, keep_daily, keep_weekly, keep_monthly, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET filesystem_id = excluded.filesystem_id, subvolume_id = excluded.subvolume_id, source_path = excluded.source_path, mountpoint = excluded.mountpoint, snapshot_root = excluded.snapshot_root, schedule = excluded.schedule, keep_hourly = excluded.keep_hourly, keep_daily = excluded.keep_daily, keep_weekly = excluded.keep_weekly, keep_monthly = excluded.keep_monthly, enabled = excluded.enabled, updated_at = CURRENT_TIMESTAMP",
            params![
                policy.id.to_string(),
                policy.filesystem_id.as_ref().map(|id| id.0.to_string()),
                policy.subvolume_id.0 as i64,
                policy.source_path.display().to_string(),
                policy.mountpoint.display().to_string(),
                policy.snapshot_root.display().to_string(),
                policy.schedule.as_str(),
                policy.keep_hourly as i64,
                policy.keep_daily as i64,
                policy.keep_weekly as i64,
                policy.keep_monthly as i64,
                i64::from(policy.enabled),
            ],
        )?;
        Ok(())
    }

    fn insert_managed_snapshot(
        &self,
        policy_id: Option<Uuid>,
        snapshot: &Snapshot,
    ) -> Result<(), HelperError> {
        let tags = serde_json::to_string(&snapshot.tags)?;
        self.connection.execute(
            "INSERT OR REPLACE INTO managed_snapshots (id, policy_id, source_subvolume_id, path, created_at, tags_json, origin_tool, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
            params![
                snapshot.id.to_string(),
                policy_id.map(|id| id.to_string()),
                snapshot.source_subvolume.0 as i64,
                snapshot.path.display().to_string(),
                snapshot.created_at.to_rfc3339(),
                tags,
                snapshot_state_to_db(&snapshot.state),
            ],
        )?;
        Ok(())
    }

    fn list_all_managed_snapshots(&self) -> Result<Vec<Snapshot>, HelperError> {
        let mut stmt = self.connection.prepare(
            "SELECT id, source_subvolume_id, path, created_at, tags_json, origin_tool, state FROM managed_snapshots ORDER BY created_at DESC",
        )?;
        let snapshots = stmt
            .query_map([], snapshot_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(snapshots)
    }

    fn list_managed_snapshot_paths(&self) -> Result<std::collections::HashSet<PathBuf>, HelperError> {
        let mut stmt = self.connection.prepare("SELECT path FROM managed_snapshots")?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }

    fn find_managed_snapshot_id_by_path(&self, path: &Path) -> Result<Uuid, HelperError> {
        let result: Option<String> = self.connection
            .query_row(
                "SELECT id FROM managed_snapshots WHERE path = ?1",
                params![path.display().to_string()],
                |row| row.get(0),
            )
            .optional()?;
        result
            .ok_or_else(|| HelperError::InvalidPolicy(format!(
                "no managed snapshot at path {}",
                path.display()
            )))
            .and_then(|id| {
                id.parse::<Uuid>().map_err(|e| {
                    HelperError::InvalidPolicy(format!("invalid uuid in db: {e}"))
                })
            })
    }

    fn delete_managed_snapshot(&self, id: Uuid) -> Result<(), HelperError> {
        self.connection.execute(
            "DELETE FROM managed_snapshots WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    fn list_managed_snapshots_for_policy(
        &self,
        policy_id: Uuid,
    ) -> Result<Vec<Snapshot>, HelperError> {
        let mut statement = self.connection.prepare(
            "SELECT id, source_subvolume_id, path, created_at, tags_json, origin_tool, state FROM managed_snapshots WHERE policy_id = ?1 ORDER BY created_at DESC",
        )?;
        let snapshots = statement
            .query_map(params![policy_id.to_string()], snapshot_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(snapshots)
    }

    fn insert_policy_log(&self, log: &PolicyRunLog) -> Result<(), HelperError> {
        let deleted = serde_json::to_string(&log.deleted_snapshots)?;
        self.connection.execute(
            "INSERT INTO policy_run_logs (id, policy_id, started_at, finished_at, status, created_snapshot, deleted_snapshots_json, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                log.id.to_string(),
                log.policy_id.to_string(),
                log.started_at.to_rfc3339(),
                log.finished_at.to_rfc3339(),
                policy_run_status_to_db(&log.status),
                log.created_snapshot.as_ref().map(|path| path.display().to_string()),
                deleted,
                log.error,
            ],
        )?;
        Ok(())
    }

    fn list_policy_logs(&self, policy_id: Uuid) -> Result<Vec<PolicyRunLog>, HelperError> {
        let mut statement = self.connection.prepare(
            "SELECT id, policy_id, started_at, finished_at, status, created_snapshot, deleted_snapshots_json, error FROM policy_run_logs WHERE policy_id = ?1 ORDER BY started_at DESC LIMIT 50",
        )?;
        let logs = statement
            .query_map(params![policy_id.to_string()], policy_log_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(logs)
    }
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), HelperError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|existing| existing == column) {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn policy_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotPolicy> {
    let id: String = row.get(0)?;
    let filesystem_id: Option<String> = row.get(1)?;
    let schedule: String = row.get(6)?;
    Ok(SnapshotPolicy {
        id: parse_uuid_for_sql(id, 0)?,
        filesystem_id: filesystem_id
            .map(|value| parse_uuid_for_sql(value, 1).map(FilesystemId))
            .transpose()?,
        subvolume_id: SubvolumeId(row.get::<_, i64>(2)? as u64),
        source_path: PathBuf::from(row.get::<_, String>(3)?),
        mountpoint: PathBuf::from(row.get::<_, String>(4)?),
        snapshot_root: PathBuf::from(row.get::<_, String>(5)?),
        schedule: schedule.parse().map_err(|err: String| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            )
        })?,
        keep_hourly: row.get::<_, i64>(7)? as usize,
        keep_daily: row.get::<_, i64>(8)? as usize,
        keep_weekly: row.get::<_, i64>(9)? as usize,
        keep_monthly: row.get::<_, i64>(10)? as usize,
        enabled: row.get::<_, i64>(11)? != 0,
    })
}

fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Snapshot> {
    let id: String = row.get(0)?;
    let created_at: String = row.get(3)?;
    let tags_json: String = row.get(4)?;
    let origin_tool: Option<String> = row.get(5)?;
    let state: String = row.get(6)?;
    Ok(Snapshot {
        id: parse_uuid_for_sql(id, 0)?,
        source_subvolume: SubvolumeId(row.get::<_, i64>(1)? as u64),
        path: PathBuf::from(row.get::<_, String>(2)?),
        created_at: parse_datetime_for_sql(created_at, 3)?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        origin: origin_tool
            .map(|tool| SnapshotOrigin::External { tool: Some(tool) })
            .unwrap_or(SnapshotOrigin::Managed),
        state: snapshot_state_from_db(&state).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            )
        })?,
    })
}

fn policy_log_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PolicyRunLog> {
    let id: String = row.get(0)?;
    let policy_id: String = row.get(1)?;
    let started_at: String = row.get(2)?;
    let finished_at: String = row.get(3)?;
    let status: String = row.get(4)?;
    let created_snapshot: Option<String> = row.get(5)?;
    let deleted_json: String = row.get(6)?;
    Ok(PolicyRunLog {
        id: parse_uuid_for_sql(id, 0)?,
        policy_id: parse_uuid_for_sql(policy_id, 1)?,
        started_at: parse_datetime_for_sql(started_at, 2)?,
        finished_at: parse_datetime_for_sql(finished_at, 3)?,
        status: policy_run_status_from_db(&status).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            )
        })?,
        created_snapshot: created_snapshot.map(PathBuf::from),
        deleted_snapshots: serde_json::from_str(&deleted_json).unwrap_or_default(),
        error: row.get(7)?,
    })
}

fn parse_uuid_for_sql(value: String, index: usize) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn parse_datetime_for_sql(value: String, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })
}

fn snapshot_state_to_db(state: &SnapshotState) -> &'static str {
    match state {
        SnapshotState::ReadOnly => "readonly",
        SnapshotState::Unlocked => "unlocked",
        SnapshotState::DirtyUnlocked => "dirty_unlocked",
        SnapshotState::RollbackAnchor => "rollback_anchor",
    }
}

fn snapshot_state_from_db(value: &str) -> Result<SnapshotState, String> {
    match value {
        "readonly" => Ok(SnapshotState::ReadOnly),
        "unlocked" => Ok(SnapshotState::Unlocked),
        "dirty_unlocked" => Ok(SnapshotState::DirtyUnlocked),
        "rollback_anchor" => Ok(SnapshotState::RollbackAnchor),
        _ => Err(format!("unknown snapshot state: {value}")),
    }
}

fn policy_run_status_to_db(status: &PolicyRunStatus) -> &'static str {
    match status {
        PolicyRunStatus::Success => "success",
        PolicyRunStatus::Failed => "failed",
    }
}

fn policy_run_status_from_db(value: &str) -> Result<PolicyRunStatus, String> {
    match value {
        "success" => Ok(PolicyRunStatus::Success),
        "failed" => Ok(PolicyRunStatus::Failed),
        _ => Err(format!("unknown policy run status: {value}")),
    }
}

fn state_db_path() -> PathBuf {
    std::env::var_os("BTRFS_MANAGER_STATE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/btrfs-manager/state.db"))
}

fn systemd_unit_dir() -> PathBuf {
    std::env::var_os("BTRFS_MANAGER_SYSTEMD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/systemd/system"))
}

fn validate_policy(policy: &SnapshotPolicy) -> Result<(), HelperError> {
    validate_path(&policy.source_path)?;
    validate_path(&policy.mountpoint)?;
    if policy.snapshot_root.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::Prefix(_)
        )
    }) {
        return Err(HelperError::InvalidPolicy(
            "snapshot root must not contain traversal".into(),
        ));
    }
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

fn policy_snapshot_dir(policy: &SnapshotPolicy) -> PathBuf {
    let root = if policy.snapshot_root.is_absolute() {
        policy.snapshot_root.clone()
    } else {
        policy.mountpoint.join(&policy.snapshot_root)
    };
    root.join("btrfs-manager").join(policy.id.to_string())
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
    )
}

fn path_looks_like_snapshot(path: &Path) -> bool {
    let text = path.to_string_lossy().to_ascii_lowercase();
    text.contains("timeshift")
        || text.contains("snapper")
        || text.contains("snapshots/")
        || text.contains(".snapshots/")
        || text.ends_with("/snapshot")
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
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        helper
            .handle(HelperRequest::MountTopLevel {
                mountpoint: "/".into(),
                target: "/tmp/top".into(),
            })
            .unwrap();
        let calls = helper.runner.calls.borrow();
        assert_eq!(calls[0].0, "findmnt");
        assert_eq!(calls[1].0, "mount");
        assert!(calls[1].1.contains(&"ro,subvolid=5".to_string()));
        assert!(calls[1].1.contains(&"/dev/mapper/cryptroot".to_string()));
        assert!(
            !calls[1]
                .1
                .contains(&"/dev/mapper/cryptroot[/@]".to_string())
        );
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
        std::fs::create_dir_all(std::env::temp_dir().join("btrfs-manager-browse")).unwrap();
        let runner = RecordingRunner {
            calls: RefCell::new(Vec::new()),
        };
        let helper = Helper::new(runner);
        helper.handle(HelperRequest::CleanupManagedMounts).unwrap();
        let calls = helper.runner.calls.borrow();
        assert!(calls.iter().any(|(program, _)| program == "umount"));
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
}
