use crate::state::StateStore;
use crate::tests::support::{
    RecordingRunner, RetentionRunner, find_snap, managed_snapshot, with_test_db,
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
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-batch-delete-{}", Uuid::new_v4()));
        let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let top = test_root.join(fs_uuid);
        std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
        }

        let state_db = top.join("@btrfs-manager/state/state.db");
        let store = StateStore::open_at(state_db.clone()).unwrap();
        let a = managed_snapshot("@btrfs-manager/managed-a", SnapshotState::ReadOnly);
        let b = managed_snapshot("@btrfs-manager/managed-b", SnapshotState::ReadOnly);
        std::fs::create_dir_all(top.join(&a.path)).unwrap();
        std::fs::create_dir_all(top.join(&b.path)).unwrap();
        store.insert_managed_snapshot(None, &a).unwrap();
        store.insert_managed_snapshot(None, &b).unwrap();

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
        let after = StateStore::open_at(state_db).unwrap();
        assert!(
            after.list_all_managed_snapshots().unwrap().is_empty(),
            "all DB rows should be removed"
        );

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(test_root).ok();
    });
}

#[test]
fn delete_managed_snapshots_reports_partial_failure() {
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-batch-partial-{}", Uuid::new_v4()));
        let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let top = test_root.join(fs_uuid);
        std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
        }

        let state_db = top.join("@btrfs-manager/state/state.db");
        let store = StateStore::open_at(state_db.clone()).unwrap();
        let good = managed_snapshot("@btrfs-manager/managed-good", SnapshotState::ReadOnly);
        std::fs::create_dir_all(top.join(&good.path)).unwrap();
        store.insert_managed_snapshot(None, &good).unwrap();

        let helper = Helper::new(RetentionRunner {
            calls: RefCell::new(Vec::new()),
        });
        // Second path has no DB row → find_managed_snapshot_id_by_path fails.
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
        let after = StateStore::open_at(state_db).unwrap();
        assert!(after.list_all_managed_snapshots().unwrap().is_empty());

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(test_root).ok();
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
