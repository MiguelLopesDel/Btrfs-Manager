use crate::subvolume::normalize_findmnt_source;
use crate::tests::support::{RecordingRunner, with_test_db};
use crate::validate::validate_managed_mount_target_with_roots;
use crate::{Helper, HelperError, HelperRequest};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

#[test]
fn mounts_top_level_with_subvolid_five() {
    with_test_db(|| {
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
        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    });
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
