use crate::tests::support::{RecordingRunner, with_runtime_dir};
use crate::{Helper, HelperError, HelperRequest};
use std::cell::RefCell;
use std::path::PathBuf;

const CALLER_UID: u32 = 1000;
const GOOD_NAME: &str = "btrfs-manager-git-0.1.0.r1.gabc123-1-x86_64.pkg.tar.zst";

fn recorder() -> RecordingRunner {
    RecordingRunner {
        calls: RefCell::new(Vec::new()),
    }
}

#[test]
fn rejects_when_caller_is_not_authenticated_via_dbus() {
    with_runtime_dir(|runtime_dir| {
        // No .with_caller_uid(...) — same state as the CLI/scheduled path.
        let helper = Helper::new(recorder());
        let err = helper
            .handle(HelperRequest::ApplySelfUpdate {
                package_path: runtime_dir.join("btrfs-manager/update").join(GOOD_NAME),
                expected_sha256: "irrelevant".into(),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(helper.runner.calls.borrow().is_empty());
    });
}

#[test]
fn rejects_package_path_outside_the_expected_update_directory() {
    with_runtime_dir(|_runtime_dir| {
        let helper = Helper::new(recorder()).with_caller_uid(CALLER_UID);
        let err = helper
            .handle(HelperRequest::ApplySelfUpdate {
                package_path: PathBuf::from("/tmp/evil.pkg.tar.zst"),
                expected_sha256: "irrelevant".into(),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(helper.runner.calls.borrow().is_empty());
    });
}

#[test]
fn rejects_filename_that_does_not_match_the_published_package_pattern() {
    with_runtime_dir(|runtime_dir| {
        let helper = Helper::new(recorder()).with_caller_uid(CALLER_UID);
        let err = helper
            .handle(HelperRequest::ApplySelfUpdate {
                package_path: runtime_dir
                    .join("btrfs-manager/update")
                    .join("not-the-right-package.pkg.tar.zst"),
                expected_sha256: "irrelevant".into(),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(helper.runner.calls.borrow().is_empty());
    });
}

#[test]
fn rejects_checksum_mismatch_and_never_calls_pacman() {
    with_runtime_dir(|runtime_dir| {
        let dir = runtime_dir.join("btrfs-manager/update");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(GOOD_NAME);
        std::fs::write(&path, b"package bytes").unwrap();

        let helper = Helper::new(recorder()).with_caller_uid(CALLER_UID);
        let err = helper
            .handle(HelperRequest::ApplySelfUpdate {
                package_path: path.clone(),
                expected_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
            })
            .unwrap_err();
        assert!(matches!(err, HelperError::InvalidPolicy(_)));
        assert!(
            helper.runner.calls.borrow().is_empty(),
            "pacman must not run when the checksum is wrong"
        );
    });
}

#[test]
fn installs_via_pacman_when_path_and_checksum_are_both_valid() {
    with_runtime_dir(|runtime_dir| {
        let dir = runtime_dir.join("btrfs-manager/update");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(GOOD_NAME);
        let content = b"package bytes";
        std::fs::write(&path, content).unwrap();
        let sha256 = btrfs_manager_core::sha256_hex(content);

        let helper = Helper::new(recorder()).with_caller_uid(CALLER_UID);
        let response = helper
            .handle(HelperRequest::ApplySelfUpdate {
                package_path: path.clone(),
                expected_sha256: sha256,
            })
            .unwrap();

        assert!(response.ok);
        let calls = helper.runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "pacman");
        assert_eq!(calls[0].1[0], "-U");
        assert_eq!(calls[0].1[1], "--noconfirm");
        assert_eq!(calls[0].1[2], path.display().to_string());
    });
}
