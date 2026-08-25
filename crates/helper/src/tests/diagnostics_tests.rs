use crate::tests::support::{DiagnosticsRunner, with_test_db};
use crate::{DiagnosticStatus, DiagnosticsReport, Helper, HelperRequest};
use uuid::Uuid;

#[test]
fn diagnostics_report_contains_core_checks() {
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-diagnostics-{}", Uuid::new_v4()));
        let top = test_root.join("550e8400-e29b-41d4-a716-446655440000");
        std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
        }

        let helper = Helper::new(DiagnosticsRunner);
        let response = helper.handle(HelperRequest::RunDiagnostics).unwrap();
        let report: DiagnosticsReport = serde_json::from_value(response.data.unwrap()).unwrap();
        let names = report
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"Helper"));
        assert!(names.contains(&"Btrfs tools"));
        assert!(names.contains(&"Root filesystem"));
        assert!(names.contains(&"Btrfs top-level mount"));
        assert!(names.contains(&"Rollback"));
        assert!(report.checks.iter().any(|check| {
            check.name == "Root filesystem" && matches!(check.status, DiagnosticStatus::Ok)
        }));

        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(test_root).ok();
    });
}
