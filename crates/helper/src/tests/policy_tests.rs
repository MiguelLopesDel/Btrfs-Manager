use crate::retention::retention_preview_for_policy;
use crate::state::StateStore;
use crate::tests::support::{FailingSystemdRunner, RetentionRunner, test_policy, with_test_db};
use crate::{Helper, HelperError, HelperRequest};
use btrfs_manager_core::models::{
    Snapshot, SnapshotOrigin, SnapshotPolicy, SnapshotState, SubvolumeId,
};
use chrono::Utc;
use std::cell::RefCell;
use std::path::PathBuf;
use uuid::Uuid;

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
