use crate::state::StateStore;
use crate::subvolume::{
    classify_subvolume_kind, mounted_subvolume_from_options, parse_default_subvolume_id,
};
use crate::tests::support::{ListReconcileRunner, RecordingRunner, managed_snapshot, with_test_db};
use crate::{FilesystemDiscovery, Helper, HelperRequest, SubvolumeInventory};
use btrfs_manager_core::models::{SnapshotState, SubvolumeId, SubvolumeKind};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[test]
fn list_subvolumes_returns_structured_inventory() {
    // Serialize on the same lock as other BTRFS_MANAGER_TOPLEVEL_DIR tests and
    // always clear the (process-global) var so it does not leak into others.
    with_test_db(|| {
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
        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    });
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
    let discovery: FilesystemDiscovery = serde_json::from_value(response.data.unwrap()).unwrap();
    assert_eq!(discovery.filesystems.len(), 1);
    let fs = &discovery.filesystems[0];
    assert_eq!(fs.mounts.len(), 2);
    assert_eq!(fs.devices, vec![PathBuf::from("/dev/mapper/cryptroot")]);
    assert_eq!(fs.default_subvolume, Some(SubvolumeId(256)));
    assert!(fs.mounts.iter().any(|mount| mount.is_active_root));
    assert_eq!(fs.mounts[0].mounted_subvolume, Some(PathBuf::from("@")));
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
fn list_subvolumes_reconciles_externally_deleted_snapshots() {
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-reconcile-{}", Uuid::new_v4()));
        let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let top = test_root.join(fs_uuid);
        std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
        }

        let state_db = top.join("@btrfs-manager/state/state.db");
        let store = StateStore::open_at(state_db.clone()).unwrap();
        let keep = managed_snapshot("@btrfs-manager/managed-keep", SnapshotState::ReadOnly);
        let stale = managed_snapshot("@btrfs-manager/managed-stale", SnapshotState::ReadOnly);
        // A rollback anchor removed from the namespace on purpose must survive.
        let anchor = managed_snapshot(
            "@btrfs-manager/managed-anchor",
            SnapshotState::RollbackAnchor,
        );
        store.insert_managed_snapshot(None, &keep).unwrap();
        store.insert_managed_snapshot(None, &stale).unwrap();
        store.insert_managed_snapshot(None, &anchor).unwrap();

        // Only "keep" is still present in the live subvolume list.
        let list_output = format!(
            "ID 256 gen 10 top level 5 uuid db14ad1b-c411-f247-8770-e8386e647b88 path {}\n",
            keep.path.display()
        );
        let helper = Helper::new(ListReconcileRunner { list_output });
        let response = helper
            .handle(HelperRequest::ListSubvolumes {
                mountpoint: PathBuf::from("/mnt"),
            })
            .unwrap();
        let inventory: SubvolumeInventory = serde_json::from_value(response.data.unwrap()).unwrap();
        assert_eq!(
            inventory.reconciled_external_deletions, 1,
            "only the non-anchor missing snapshot should be reconciled"
        );

        let after = StateStore::open_at(state_db).unwrap();
        let paths: Vec<PathBuf> = after
            .list_all_managed_snapshots()
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert!(paths.contains(&keep.path), "present snapshot kept");
        assert!(
            !paths.contains(&stale.path),
            "externally deleted snapshot pruned"
        );
        assert!(
            paths.contains(&anchor.path),
            "rollback anchor must not be pruned"
        );

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(test_root).ok();
    });
}

#[test]
fn list_subvolumes_does_not_prune_when_listing_is_empty() {
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-reconcile-empty-{}", Uuid::new_v4()));
        let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let top = test_root.join(fs_uuid);
        std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
        }

        let state_db = top.join("@btrfs-manager/state/state.db");
        let store = StateStore::open_at(state_db.clone()).unwrap();
        let snap = managed_snapshot("@btrfs-manager/managed-x", SnapshotState::ReadOnly);
        store.insert_managed_snapshot(None, &snap).unwrap();

        // An empty (unreliable) live listing must NOT wipe managed metadata.
        let helper = Helper::new(ListReconcileRunner {
            list_output: String::new(),
        });
        let response = helper
            .handle(HelperRequest::ListSubvolumes {
                mountpoint: PathBuf::from("/mnt"),
            })
            .unwrap();
        let inventory: SubvolumeInventory = serde_json::from_value(response.data.unwrap()).unwrap();
        assert_eq!(
            inventory.reconciled_external_deletions, 0,
            "empty listing must not trigger any pruning"
        );

        let after = StateStore::open_at(state_db).unwrap();
        assert_eq!(
            after.list_all_managed_snapshots().unwrap().len(),
            1,
            "managed metadata must survive an empty listing"
        );

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(test_root).ok();
    });
}
