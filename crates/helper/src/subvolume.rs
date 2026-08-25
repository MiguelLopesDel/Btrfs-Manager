use crate::boot::detect_boot_integration;
use crate::validate::validate_path;
use crate::{
    CommandRunner, FilesystemDiscovery, Helper, HelperError, HelperResponse, SubvolumeInventory,
};
use btrfs_manager_core::models::{
    FilesystemId, FilesystemMount, FilesystemSummary, Snapshot, SnapshotOrigin, SnapshotState,
    Subvolume, SubvolumeId, SubvolumeKind,
};
use btrfs_manager_core::parser::{parse_btrfs_subvolume_list, parse_findmnt_pairs};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn list_subvolumes(
        &self,
        mountpoint: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
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
        let mut reconciled_external_deletions = 0usize;
        if let Some(store) = Self::existing_state_store_at_top_level(&top)? {
            if let Ok(snapshots) = store.list_all_managed_snapshots() {
                // Reconcile: prune DB rows whose subvolume was deleted outside the
                // app. Rollback anchors are removed from the namespace on purpose
                // by StageRollback while awaiting reboot, so never prune those.
                //
                // Guard: never prune when the live listing is empty. A real Btrfs
                // volume that holds managed snapshots always lists at least those
                // subvolumes; an empty result means the listing is unreliable
                // (e.g. a stale/empty top-level mount), and pruning everything
                // would destroy managed metadata (tags, unlock state, timestamps).
                if !subvolumes.is_empty() {
                    let real_paths: std::collections::HashSet<&Path> =
                        subvolumes.iter().map(|s| s.path.as_path()).collect();
                    for snap in &snapshots {
                        if real_paths.contains(snap.path.as_path())
                            || matches!(snap.state, SnapshotState::RollbackAnchor)
                        {
                            continue;
                        }
                        match store.delete_managed_snapshot(snap.id) {
                            Ok(()) => {
                                tracing::info!(
                                    path = %snap.path.display(),
                                    "pruned stale managed snapshot (deleted externally)"
                                );
                                reconciled_external_deletions += 1;
                            }
                            Err(err) => tracing::warn!(
                                path = %snap.path.display(),
                                error = %err,
                                "failed to prune stale managed snapshot"
                            ),
                        }
                    }
                }
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
            reconciled_external_deletions,
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

    pub(crate) fn discover_filesystems(&self) -> Result<HelperResponse, HelperError> {
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

    pub(crate) fn default_subvolume_for_mount(
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
}

pub(crate) fn snapshots_from_subvolumes(subvolumes: &[Subvolume]) -> Vec<Snapshot> {
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
            created_at: chrono::Utc::now(),
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

pub(crate) fn classify_subvolumes(subvolumes: &mut [Subvolume]) {
    for subvolume in subvolumes {
        subvolume.kind = classify_subvolume_kind(&subvolume.path);
    }
}

pub(crate) fn classify_subvolume_kind(path: &Path) -> SubvolumeKind {
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

pub(crate) fn normalize_findmnt_source(source: &str) -> String {
    source
        .split_once('[')
        .map(|(device, _)| device)
        .unwrap_or(source)
        .to_string()
}

pub(crate) fn mounted_subvolume_from_options(options: &str) -> Option<PathBuf> {
    options.split(',').find_map(|option| {
        option
            .strip_prefix("subvol=")
            .map(|subvolume| PathBuf::from(subvolume.trim_start_matches('/')))
    })
}

pub(crate) fn parse_default_subvolume_id(output: &str) -> Option<SubvolumeId> {
    let mut tokens = output.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "ID" {
            return tokens.next()?.parse::<u64>().ok().map(SubvolumeId);
        }
    }
    None
}
