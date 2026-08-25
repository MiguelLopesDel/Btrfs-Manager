use crate::rollback::{RollbackPlanFileState, read_rollback_plan_file_state};
use crate::subvolume::mounted_subvolume_from_options;
use crate::{
    CommandRunner, DiagnosticCheck, DiagnosticStatus, DiagnosticsReport, Helper, HelperError,
    HelperResponse,
};
use btrfs_manager_core::parser::parse_findmnt_pairs;
use chrono::Utc;
use std::path::Path;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn run_diagnostics(&self) -> Result<HelperResponse, HelperError> {
        let mut checks = vec![
            diagnostic_check(
                "Helper",
                DiagnosticStatus::Ok,
                "Privileged helper responded to the diagnostics request",
                Vec::new(),
            ),
            self.btrfs_tools_diagnostic(),
            self.system_service_diagnostic(),
            polkit_policy_diagnostic(),
            self.root_filesystem_diagnostic(),
        ];

        match self.ensure_top_level_mount(Path::new("/")) {
            Ok(top) => {
                checks.push(diagnostic_check(
                    "Btrfs top-level mount",
                    DiagnosticStatus::Ok,
                    "Top-level Btrfs mount is available",
                    vec![top.display().to_string()],
                ));
                checks.extend(self.manager_state_diagnostics(&top));
                checks.extend(self.policy_timer_diagnostics(&top));
                checks.push(self.rollback_diagnostic(&top));
            }
            Err(err) => checks.push(diagnostic_check(
                "Btrfs top-level mount",
                DiagnosticStatus::Error,
                "Could not mount or find the Btrfs top-level subvolume",
                vec![err.to_string()],
            )),
        }

        let report = DiagnosticsReport {
            generated_at: Utc::now(),
            checks,
        };
        Ok(HelperResponse {
            ok: true,
            message: "diagnostics completed".into(),
            data: Some(serde_json::to_value(report)?),
        })
    }

    fn btrfs_tools_diagnostic(&self) -> DiagnosticCheck {
        match self.runner.run("btrfs", &["version".into()]) {
            Ok(output) => diagnostic_check(
                "Btrfs tools",
                DiagnosticStatus::Ok,
                output.trim(),
                Vec::new(),
            ),
            Err(err) => diagnostic_check(
                "Btrfs tools",
                DiagnosticStatus::Error,
                "btrfs-progs is not available or failed to run",
                vec![err.to_string()],
            ),
        }
    }

    fn system_service_diagnostic(&self) -> DiagnosticCheck {
        match self.runner.run(
            "systemctl",
            &["is-active".into(), "btrfs-manager-helper.service".into()],
        ) {
            Ok(output) if output.trim() == "active" => diagnostic_check(
                "System service",
                DiagnosticStatus::Ok,
                "btrfs-manager-helper.service is active",
                Vec::new(),
            ),
            Ok(output) => diagnostic_check(
                "System service",
                DiagnosticStatus::Warning,
                "btrfs-manager-helper.service is installed but not active",
                vec![format!("systemctl is-active returned {}", output.trim())],
            ),
            Err(err) => diagnostic_check(
                "System service",
                DiagnosticStatus::Warning,
                "Could not query btrfs-manager-helper.service",
                vec![err.to_string()],
            ),
        }
    }

    fn root_filesystem_diagnostic(&self) -> DiagnosticCheck {
        let root_info = self.runner.run(
            "findmnt",
            &[
                "-P".into(),
                "-n".into(),
                "-o".into(),
                "FSTYPE,SOURCE,TARGET,OPTIONS".into(),
                "--target".into(),
                "/".into(),
            ],
        );
        match root_info {
            Ok(output) => {
                let pairs = parse_findmnt_pairs(output.trim());
                let fstype = pairs.get("FSTYPE").map(String::as_str).unwrap_or("unknown");
                let source = pairs.get("SOURCE").map(String::as_str).unwrap_or("unknown");
                let options = pairs.get("OPTIONS").map(String::as_str).unwrap_or("");
                if fstype == "btrfs" {
                    let subvol = mounted_subvolume_from_options(options)
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "not reported".into());
                    diagnostic_check(
                        "Root filesystem",
                        DiagnosticStatus::Ok,
                        "Root is mounted from Btrfs",
                        vec![format!("source: {source}"), format!("subvolume: {subvol}")],
                    )
                } else {
                    diagnostic_check(
                        "Root filesystem",
                        DiagnosticStatus::Error,
                        "Root is not mounted from Btrfs",
                        vec![format!("fstype: {fstype}"), format!("source: {source}")],
                    )
                }
            }
            Err(err) => diagnostic_check(
                "Root filesystem",
                DiagnosticStatus::Error,
                "Could not inspect the root filesystem",
                vec![err.to_string()],
            ),
        }
    }

    pub(crate) fn manager_state_diagnostics(&self, top: &Path) -> Vec<DiagnosticCheck> {
        let manager = top.join("@btrfs-manager");
        if !manager.exists() {
            return vec![diagnostic_check(
                "Manager state",
                DiagnosticStatus::Warning,
                "Manager state subvolume has not been initialized yet",
                vec![manager.display().to_string()],
            )];
        }
        let mut checks = Vec::new();
        match self.runner.run(
            "btrfs",
            &[
                "subvolume".into(),
                "show".into(),
                manager.display().to_string(),
            ],
        ) {
            Ok(_) => checks.push(diagnostic_check(
                "Manager state",
                DiagnosticStatus::Ok,
                "@btrfs-manager is a Btrfs subvolume",
                vec![manager.display().to_string()],
            )),
            Err(err) => checks.push(diagnostic_check(
                "Manager state",
                DiagnosticStatus::Error,
                "@btrfs-manager exists but is not a Btrfs subvolume",
                vec![err.to_string()],
            )),
        }
        let db_path = crate::infra::state_store_path_at_top_level(top);
        checks.push(if db_path.exists() {
            diagnostic_check(
                "State database",
                DiagnosticStatus::Ok,
                "State database exists",
                vec![db_path.display().to_string()],
            )
        } else {
            diagnostic_check(
                "State database",
                DiagnosticStatus::Warning,
                "State database has not been created yet",
                vec![db_path.display().to_string()],
            )
        });
        checks
    }

    pub(crate) fn policy_timer_diagnostics(&self, top: &Path) -> Vec<DiagnosticCheck> {
        let Ok(Some(store)) = Self::existing_state_store_at_top_level(top) else {
            return vec![diagnostic_check(
                "Snapshot policies",
                DiagnosticStatus::Warning,
                "No state database is available for policy checks",
                Vec::new(),
            )];
        };
        let policies = match store.list_policies() {
            Ok(policies) => policies,
            Err(err) => {
                return vec![diagnostic_check(
                    "Snapshot policies",
                    DiagnosticStatus::Error,
                    "Could not read snapshot policies",
                    vec![err.to_string()],
                )];
            }
        };
        if policies.is_empty() {
            return vec![diagnostic_check(
                "Snapshot policies",
                DiagnosticStatus::Ok,
                "No scheduled snapshot policies configured",
                Vec::new(),
            )];
        }
        let enabled = policies.iter().filter(|policy| policy.enabled).count();
        let mut details = vec![format!("{} policy(s), {enabled} enabled", policies.len())];
        let mut status = DiagnosticStatus::Ok;
        for policy in policies.iter().filter(|policy| policy.enabled) {
            let timer = format!("btrfs-manager-policy-{}.timer", policy.id);
            match self
                .runner
                .run("systemctl", &["is-enabled".into(), timer.clone()])
            {
                Ok(output) if output.trim() == "enabled" => {
                    details.push(format!("{timer}: enabled"));
                }
                Ok(output) => {
                    status = DiagnosticStatus::Warning;
                    details.push(format!("{timer}: {}", output.trim()));
                }
                Err(err) => {
                    status = DiagnosticStatus::Warning;
                    details.push(format!("{timer}: {err}"));
                }
            }
        }
        vec![diagnostic_check(
            "Snapshot policies",
            status,
            "Scheduled policy timers checked",
            details,
        )]
    }

    pub(crate) fn rollback_diagnostic(&self, top: &Path) -> DiagnosticCheck {
        match read_rollback_plan_file_state(top) {
            Ok(RollbackPlanFileState::Pending(plan)) => diagnostic_check(
                "Rollback",
                DiagnosticStatus::Warning,
                "A rollback is pending confirmation",
                vec![
                    format!("plan: {}", plan.id),
                    format!("source: {}", plan.source_snapshot_path.display()),
                    format!("return anchor: {}", plan.return_snapshot_path.display()),
                ],
            ),
            Ok(RollbackPlanFileState::Resolved | RollbackPlanFileState::Missing) => {
                diagnostic_check(
                    "Rollback",
                    DiagnosticStatus::Ok,
                    "No pending rollback",
                    Vec::new(),
                )
            }
            Err(err) => diagnostic_check(
                "Rollback",
                DiagnosticStatus::Error,
                "Could not read rollback state",
                vec![err.to_string()],
            ),
        }
    }
}

fn polkit_policy_diagnostic() -> DiagnosticCheck {
    let polkit_path = Path::new("/usr/share/polkit-1/actions/org.btrfsmanager.helper.policy");
    if polkit_path.exists() {
        diagnostic_check(
            "Polkit policy",
            DiagnosticStatus::Ok,
            "Polkit policy is installed",
            vec![polkit_path.display().to_string()],
        )
    } else {
        diagnostic_check(
            "Polkit policy",
            DiagnosticStatus::Warning,
            "Polkit policy file was not found",
            vec![polkit_path.display().to_string()],
        )
    }
}

pub(crate) fn diagnostic_check(
    name: impl Into<String>,
    status: DiagnosticStatus,
    message: impl Into<String>,
    details: Vec<String>,
) -> DiagnosticCheck {
    DiagnosticCheck {
        name: name.into(),
        status,
        message: message.into(),
        details,
    }
}
