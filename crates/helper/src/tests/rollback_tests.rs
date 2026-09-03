use crate::state::StateStore;
use crate::tests::support::{RecordingRunner, RollbackRunner, with_test_db};
use crate::{Helper, HelperError, HelperRequest};
use btrfs_manager_core::models::SnapshotState;
use btrfs_manager_core::rollback::{RollbackPlan, RollbackPrompt, RollbackStatus};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use uuid::Uuid;

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

/// State threaded between the phases of
/// `rollback_stage_and_revert_preserve_return_anchor`.
struct RollbackStageContext {
    top: PathBuf,
    test_root: PathBuf,
    helper: Helper<RollbackRunner>,
    plan: RollbackPlan,
    plan_file: PathBuf,
    store: StateStore,
}

/// Phase 1: stage a rollback and verify the anchor/plan/DB state it leaves
/// behind.
fn stage_and_verify_initial() -> RollbackStageContext {
    let test_root = std::env::temp_dir().join(format!("btrfs-manager-rollback-{}", Uuid::new_v4()));
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

    RollbackStageContext {
        top,
        test_root,
        helper,
        plan,
        plan_file,
        store,
    }
}

/// Phase 2: after a simulated reboot (boot id changes), pending-rollback
/// lookup must report `rebooted_since_staging`.
fn assert_reboot_detection_via_boot_id(ctx: &RollbackStageContext) {
    unsafe {
        std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-after-rollback");
    }
    let pending_response = ctx
        .helper
        .handle(HelperRequest::GetPendingRollback)
        .unwrap();
    let prompt: RollbackPrompt = serde_json::from_value(pending_response.data.unwrap()).unwrap();
    assert!(prompt.rebooted_since_staging);
}

/// Phase 3: simulate a fresh boot with no local state DB — pending-rollback
/// lookup must fall back to the on-disk plan file and still find the plan.
fn assert_fallback_to_plan_file(ctx: &RollbackStageContext) {
    unsafe {
        std::env::set_var(
            "BTRFS_MANAGER_STATE_DB",
            ctx.test_root.join("restored-root-empty-state.db"),
        );
    }
    let fallback_response = ctx
        .helper
        .handle(HelperRequest::GetPendingRollback)
        .unwrap();
    let fallback_prompt: RollbackPrompt =
        serde_json::from_value(fallback_response.data.unwrap()).unwrap();
    assert_eq!(fallback_prompt.plan.id, ctx.plan.id);
    assert!(fallback_prompt.rebooted_since_staging);
}

/// Phase 4: revert the rollback and verify the return anchor is restored and
/// all pending-rollback state is cleared.
fn revert_and_verify_cleanup(ctx: &RollbackStageContext) {
    ctx.helper
        .handle(HelperRequest::RevertRollback {
            plan_id: ctx.plan.id,
        })
        .unwrap();
    assert!(
        ctx.top.join("@/etc/original.conf").exists(),
        "revert should restore the return anchor to the active root path"
    );
    let file_plan: RollbackPlan =
        serde_json::from_slice(&std::fs::read(&ctx.plan_file).unwrap()).unwrap();
    assert!(matches!(file_plan.status, RollbackStatus::Reverted));
    assert!(ctx.store.get_pending_rollback().unwrap().is_none());
    let no_pending_after_revert = ctx
        .helper
        .handle(HelperRequest::GetPendingRollback)
        .unwrap();
    assert!(no_pending_after_revert.data.is_none());
}

#[test]
fn rollback_stage_and_revert_preserve_return_anchor() {
    with_test_db(|| {
        unsafe {
            std::env::set_var("BTRFS_MANAGER_BOOT_ID", "boot-before-rollback");
        }
        let ctx = stage_and_verify_initial();
        assert_reboot_detection_via_boot_id(&ctx);
        assert_fallback_to_plan_file(&ctx);
        revert_and_verify_cleanup(&ctx);

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_BOOT_ID");
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(&ctx.test_root).ok();
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
