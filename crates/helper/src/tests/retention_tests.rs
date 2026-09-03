use crate::retention::policy_snapshot_dir;
use crate::state::StateStore;
use crate::tests::support::{RetentionRunner, test_policy, with_test_db};
use crate::{Helper, HelperRequest};
use btrfs_manager_core::models::{
    Snapshot, SnapshotOrigin, SnapshotPolicy, SnapshotState, SubvolumeId,
};
use btrfs_manager_core::{PolicyRunLog, PolicyRunStatus};
use chrono::{TimeZone, Utc};
use std::cell::RefCell;
use std::path::PathBuf;
use uuid::Uuid;

/// Captured, plain-data result of running one scheduled retention scenario:
/// a policy is created with one snapshot to keep, one to expire, and one
/// rollback anchor; the retention policy is run once. Everything a test
/// might want to assert on is pre-computed here (filesystem existence
/// checks, reopened DB rows, policy logs, recorded command shapes) so the
/// individual `#[test]` functions never need to touch the filesystem or
/// database themselves — the temp directory is already cleaned up by the
/// time this returns.
struct ScheduledRetentionOutcome {
    log: PolicyRunLog,
    created: PathBuf,
    keep_path: PathBuf,
    expired_path: PathBuf,
    anchor_path: PathBuf,
    created_exists: bool,
    keep_exists: bool,
    expired_exists: bool,
    anchor_exists: bool,
    remaining_paths: Vec<PathBuf>,
    logs: Vec<PolicyRunLog>,
    calls: Vec<(String, Vec<String>)>,
}

/// Builds the keep/expired/anchor snapshot fixtures for
/// `run_scheduled_retention_scenario`, anchored at `old_bucket`.
fn keep_expired_anchor_snapshots(
    policy: &SnapshotPolicy,
    old_bucket: chrono::DateTime<Utc>,
) -> (Snapshot, Snapshot, Snapshot) {
    let keep = Snapshot {
        id: Uuid::new_v4(),
        source_subvolume: SubvolumeId(256),
        path: policy_snapshot_dir(policy).join("keep-current-hour"),
        created_at: old_bucket,
        tags: Vec::new(),
        origin: SnapshotOrigin::Managed,
        state: SnapshotState::ReadOnly,
    };
    let expired = Snapshot {
        id: Uuid::new_v4(),
        source_subvolume: SubvolumeId(256),
        path: policy_snapshot_dir(policy).join("delete-older-hour"),
        created_at: old_bucket - chrono::Duration::minutes(10),
        tags: Vec::new(),
        origin: SnapshotOrigin::Managed,
        state: SnapshotState::ReadOnly,
    };
    let anchor = Snapshot {
        id: Uuid::new_v4(),
        source_subvolume: SubvolumeId(256),
        path: policy_snapshot_dir(policy).join("keep-anchor"),
        created_at: old_bucket - chrono::Duration::hours(3),
        tags: Vec::new(),
        origin: SnapshotOrigin::Managed,
        state: SnapshotState::RollbackAnchor,
    };
    (keep, expired, anchor)
}

fn run_scheduled_retention_scenario() -> ScheduledRetentionOutcome {
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
        let (keep, expired, anchor) = keep_expired_anchor_snapshots(&policy, old_bucket);
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
        let created = log
            .created_snapshot
            .clone()
            .expect("run should create a snapshot");

        let created_exists = top.join(&created).exists();
        let keep_exists = top.join(&keep.path).exists();
        let expired_exists = top.join(&expired.path).exists();
        let anchor_exists = top.join(&anchor.path).exists();

        let after = StateStore::open_at(state_db).unwrap();
        let remaining_paths: Vec<_> = after
            .list_managed_snapshots_for_policy(policy.id)
            .unwrap()
            .into_iter()
            .map(|snapshot| snapshot.path)
            .collect();
        let logs = after.list_policy_logs(policy.id).unwrap();
        let calls = helper.runner.calls.borrow().clone();

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(test_root).ok();

        ScheduledRetentionOutcome {
            log,
            created,
            keep_path: keep.path,
            expired_path: expired.path,
            anchor_path: anchor.path,
            created_exists,
            keep_exists,
            expired_exists,
            anchor_exists,
            remaining_paths,
            logs,
            calls,
        }
    })
}

#[test]
fn scheduled_retention_run_creates_snapshot_deletes_expired_and_logs_result() {
    let outcome = run_scheduled_retention_scenario();
    assert!(matches!(outcome.log.status, PolicyRunStatus::Success));
    assert_eq!(
        outcome.log.deleted_snapshots,
        vec![outcome.expired_path.clone()]
    );
}

#[test]
fn scheduled_retention_run_updates_filesystem_state() {
    let outcome = run_scheduled_retention_scenario();
    assert!(outcome.created_exists, "new snapshot should exist");
    assert!(
        outcome.keep_exists,
        "newest retained snapshot should remain"
    );
    assert!(
        !outcome.expired_exists,
        "expired snapshot should be deleted"
    );
    assert!(
        outcome.anchor_exists,
        "rollback anchor must never be deleted by retention"
    );
}

#[test]
fn scheduled_retention_run_persists_db_rows_and_command_shapes() {
    let outcome = run_scheduled_retention_scenario();
    assert!(outcome.remaining_paths.contains(&outcome.created));
    assert!(outcome.remaining_paths.contains(&outcome.keep_path));
    assert!(outcome.remaining_paths.contains(&outcome.anchor_path));
    assert!(!outcome.remaining_paths.contains(&outcome.expired_path));

    assert_eq!(outcome.logs.len(), 1);
    assert!(matches!(outcome.logs[0].status, PolicyRunStatus::Success));
    assert_eq!(
        outcome.logs[0].deleted_snapshots,
        vec![outcome.expired_path.clone()]
    );

    assert!(outcome.calls.iter().any(|(program, args)| {
        program == "btrfs"
            && args.first().map(String::as_str) == Some("subvolume")
            && args.get(1).map(String::as_str) == Some("snapshot")
    }));
    assert!(outcome.calls.iter().any(|(program, args)| {
        program == "btrfs"
            && args.first().map(String::as_str) == Some("subvolume")
            && args.get(1).map(String::as_str) == Some("delete")
            && args
                .get(2)
                .is_some_and(|path| path.ends_with("delete-older-hour"))
    }));
}

#[test]
fn scheduled_retention_run_removes_stale_db_entries_for_missing_snapshots() {
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-retention-stale-{}", Uuid::new_v4()));
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
