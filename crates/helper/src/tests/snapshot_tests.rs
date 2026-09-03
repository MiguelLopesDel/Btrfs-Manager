use crate::state::StateStore;
use crate::tests::support::{
    RecordingRunner, RetentionRunner, find_snap, place_managed_snapshot, place_rollback_anchor,
    with_test_db, with_top_level_fixture,
};
use crate::{Helper, HelperError, HelperRequest};
use btrfs_manager_core::models::{Snapshot, SnapshotOrigin, SnapshotState, SubvolumeId};
use chrono::Utc;
use std::cell::RefCell;
use std::path::PathBuf;
use uuid::Uuid;

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
        let tmp =
            std::env::temp_dir().join(format!("btrfs-manager-readonly-state-{}", Uuid::new_v4()));
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

#[test]
fn delete_managed_snapshots_removes_all_subvolumes_and_db_rows() {
    with_top_level_fixture("batch-delete", |top, store| {
        let a = place_managed_snapshot(store, top, "@btrfs-manager/a", SnapshotState::ReadOnly);
        let b = place_managed_snapshot(store, top, "@btrfs-manager/b", SnapshotState::ReadOnly);

        let helper = Helper::new(RetentionRunner {
            calls: RefCell::new(Vec::new()),
        });
        let response = helper
            .handle(HelperRequest::DeleteManagedSnapshots {
                mountpoint: PathBuf::from("/mnt"),
                subvolume_paths: vec![a.path.clone(), b.path.clone()],
            })
            .unwrap();

        assert!(
            response.ok,
            "batch delete should succeed: {}",
            response.message
        );
        assert!(!top.join(&a.path).exists());
        assert!(!top.join(&b.path).exists());
        assert!(
            store.list_all_managed_snapshots().unwrap().is_empty(),
            "all DB rows should be removed"
        );
    });
}

#[test]
fn delete_managed_snapshots_reports_partial_failure() {
    with_top_level_fixture("batch-partial", |top, store| {
        let good =
            place_managed_snapshot(store, top, "@btrfs-manager/good", SnapshotState::ReadOnly);

        let helper = Helper::new(RetentionRunner {
            calls: RefCell::new(Vec::new()),
        });
        // Second path has no DB row → find_managed_snapshot_by_path fails.
        let response = helper
            .handle(HelperRequest::DeleteManagedSnapshots {
                mountpoint: PathBuf::from("/mnt"),
                subvolume_paths: vec![good.path.clone(), PathBuf::from("@btrfs-manager/not-in-db")],
            })
            .unwrap();

        assert!(!response.ok, "partial failure must report not-ok");
        let failed = response.data.unwrap()["failed"].as_array().unwrap().len();
        assert_eq!(failed, 1, "exactly one path should fail");
        // The valid one was still deleted.
        assert!(!top.join(&good.path).exists());
        assert!(store.list_all_managed_snapshots().unwrap().is_empty());
    });
}

#[test]
fn delete_managed_snapshot_rejects_rollback_anchor() {
    with_top_level_fixture("anchor-delete", |top, store| {
        let anchor = place_rollback_anchor(store, top);

        let helper = Helper::new(RetentionRunner {
            calls: RefCell::new(Vec::new()),
        });
        let err = helper
            .handle(HelperRequest::DeleteManagedSnapshot {
                mountpoint: PathBuf::from("/mnt"),
                subvolume_path: anchor.path.clone(),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        // Neither the subvolume nor its DB row were touched.
        assert!(top.join(&anchor.path).exists());
        assert_eq!(store.list_all_managed_snapshots().unwrap().len(), 1);
    });
}

#[test]
fn delete_managed_snapshots_batch_skips_rollback_anchor_but_deletes_the_rest() {
    with_top_level_fixture("anchor-batch", |top, store| {
        let anchor = place_rollback_anchor(store, top);
        let ordinary = place_managed_snapshot(
            store,
            top,
            "@btrfs-manager/ordinary",
            SnapshotState::ReadOnly,
        );

        let helper = Helper::new(RetentionRunner {
            calls: RefCell::new(Vec::new()),
        });
        let response = helper
            .handle(HelperRequest::DeleteManagedSnapshots {
                mountpoint: PathBuf::from("/mnt"),
                subvolume_paths: vec![anchor.path.clone(), ordinary.path.clone()],
            })
            .unwrap();

        assert!(
            !response.ok,
            "batch containing the anchor must report not-ok"
        );
        let failed = response.data.unwrap()["failed"].as_array().unwrap().len();
        assert_eq!(failed, 1, "only the anchor should fail");
        // The anchor survives; the ordinary snapshot was still deleted.
        assert!(top.join(&anchor.path).exists());
        assert!(!top.join(&ordinary.path).exists());
        assert_eq!(store.list_all_managed_snapshots().unwrap().len(), 1);
    });
}

#[test]
fn set_managed_snapshot_ro_rejects_rollback_anchor() {
    with_top_level_fixture("anchor-lock", |top, store| {
        let anchor = place_rollback_anchor(store, top);

        let helper = Helper::new(RetentionRunner {
            calls: RefCell::new(Vec::new()),
        });
        let err = helper
            .handle(HelperRequest::SetManagedSnapshotReadOnly {
                mountpoint: PathBuf::from("/mnt"),
                subvol_path: anchor.path.clone(),
                readonly: false,
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(matches!(
            find_snap(store, anchor.id).state,
            SnapshotState::RollbackAnchor
        ));
    });
}

#[test]
fn delete_managed_snapshot_rejects_unsafe_path_before_mounting() {
    let runner = RecordingRunner {
        calls: RefCell::new(Vec::new()),
    };
    let helper = Helper::new(runner);
    let err = helper
        .handle(HelperRequest::DeleteManagedSnapshot {
            mountpoint: PathBuf::from("/mnt"),
            subvolume_path: PathBuf::from("@btrfs-manager/../../etc"),
        })
        .unwrap_err();
    assert!(
        matches!(err, HelperError::InvalidPolicy(_)),
        "traversal path must be rejected, got {err}"
    );
    // Fail-fast: no privileged command (findmnt/mount/btrfs) may run first.
    assert!(
        helper.runner.calls.borrow().is_empty(),
        "no command should run before path validation"
    );
}
