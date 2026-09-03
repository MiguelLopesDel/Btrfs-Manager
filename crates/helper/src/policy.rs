use crate::state::StateStore;
use crate::validate::{validate_path, validate_relative_btrfs_path};
use crate::{CommandRunner, Helper, HelperError, HelperResponse};
use btrfs_manager_core::models::SnapshotPolicy;
use std::path::PathBuf;
use uuid::Uuid;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn list_snapshot_policies_impl(&self) -> Result<HelperResponse, HelperError> {
        let policies = self.default_state_store()?.list_policies()?;
        Ok(HelperResponse {
            ok: true,
            message: format!("found {} snapshot policies", policies.len()),
            data: Some(serde_json::to_value(policies)?),
        })
    }

    pub(crate) fn upsert_snapshot_policy_impl(
        &self,
        policy: SnapshotPolicy,
    ) -> Result<HelperResponse, HelperError> {
        validate_policy(&policy)?;
        let store = self.default_state_store()?;
        let previous = store.get_policy(policy.id)?;
        store.upsert_policy(&policy)?;
        if let Err(err) = self.write_systemd_policy_units(&policy) {
            self.recover_policy_after_scheduler_failure(&store, previous.as_ref(), &policy);
            return Err(err);
        }
        Ok(HelperResponse {
            ok: true,
            message: "snapshot policy saved".into(),
            data: Some(serde_json::to_value(policy)?),
        })
    }

    pub(crate) fn set_snapshot_policy_enabled_impl(
        &self,
        policy_id: Uuid,
        enabled: bool,
    ) -> Result<HelperResponse, HelperError> {
        let store = self.default_state_store()?;
        let mut policy = store
            .get_policy(policy_id)?
            .ok_or_else(|| HelperError::InvalidPolicy(format!("unknown policy {policy_id}")))?;
        let previous = policy.clone();
        policy.enabled = enabled;
        store.upsert_policy(&policy)?;
        if let Err(err) = self.write_systemd_policy_units(&policy) {
            self.recover_policy_after_scheduler_failure(&store, Some(&previous), &policy);
            return Err(err);
        }
        Ok(HelperResponse {
            ok: true,
            message: if enabled {
                "snapshot policy enabled".into()
            } else {
                "snapshot policy disabled".into()
            },
            data: Some(serde_json::to_value(policy)?),
        })
    }

    pub(crate) fn recover_policy_after_scheduler_failure(
        &self,
        store: &StateStore,
        previous: Option<&SnapshotPolicy>,
        attempted: &SnapshotPolicy,
    ) {
        let restore_result = match previous {
            Some(policy) => store.upsert_policy(policy),
            None => store.delete_policy(attempted.id),
        };
        if let Err(err) = restore_result {
            tracing::error!(
                policy_id = %attempted.id,
                error = %err,
                "failed to restore policy state after scheduler setup failure"
            );
        }

        let unit_result = match previous {
            Some(policy) => self.write_systemd_policy_units(policy),
            None => self.remove_systemd_policy_units(attempted.id),
        };
        if let Err(err) = unit_result {
            tracing::warn!(
                policy_id = %attempted.id,
                error = %err,
                "failed to restore scheduler unit files after scheduler setup failure"
            );
        }
    }

    pub(crate) fn write_systemd_policy_units(
        &self,
        policy: &SnapshotPolicy,
    ) -> Result<(), HelperError> {
        let unit_dir = systemd_unit_dir();
        std::fs::create_dir_all(&unit_dir)?;
        let service_path = unit_dir.join(format!("btrfs-manager-policy-{}.service", policy.id));
        let timer_path = unit_dir.join(format!("btrfs-manager-policy-{}.timer", policy.id));
        let service_name = service_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| HelperError::InvalidPolicy("invalid service unit name".into()))?;
        let timer_name = timer_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| HelperError::InvalidPolicy("invalid timer unit name".into()))?;

        std::fs::write(
            &service_path,
            format!(
                "[Unit]\nDescription=Btrfs Manager snapshot policy {}\n\n[Service]\nType=oneshot\nExecStart=/usr/lib/btrfs-manager/btrfs-manager-helper run-retention-policy --policy-id {}\n",
                policy.id, policy.id
            ),
        )?;
        std::fs::write(
            &timer_path,
            format!(
                "[Unit]\nDescription=Btrfs Manager scheduled snapshot policy {}\n\n[Timer]\nOnCalendar={}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
                policy.id,
                policy.schedule.as_str()
            ),
        )?;

        self.runner.run("systemctl", &["daemon-reload".into()])?;
        if policy.enabled {
            self.runner.run(
                "systemctl",
                &["enable".into(), "--now".into(), timer_name.into()],
            )?;
        } else {
            self.runner.run(
                "systemctl",
                &["disable".into(), "--now".into(), timer_name.into()],
            )?;
        }
        let _ = service_name;
        Ok(())
    }

    pub(crate) fn remove_systemd_policy_units(&self, policy_id: Uuid) -> Result<(), HelperError> {
        let unit_dir = systemd_unit_dir();
        for extension in ["service", "timer"] {
            let path = unit_dir.join(format!("btrfs-manager-policy-{policy_id}.{extension}"));
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        self.runner.run("systemctl", &["daemon-reload".into()])?;
        Ok(())
    }
}

pub(crate) fn systemd_unit_dir() -> PathBuf {
    std::env::var_os("BTRFS_MANAGER_SYSTEMD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/systemd/system"))
}

pub(crate) fn validate_policy(policy: &SnapshotPolicy) -> Result<(), HelperError> {
    // source_path and snapshot_root are relative to the Btrfs volume root.
    // mountpoint is an absolute path used to find the block device.
    validate_path(&policy.mountpoint)?;
    validate_relative_btrfs_path(&policy.source_path, "policy source path")?;
    validate_relative_btrfs_path(&policy.snapshot_root, "policy snapshot root")?;
    if policy.keep_hourly + policy.keep_daily + policy.keep_weekly + policy.keep_monthly == 0 {
        return Err(HelperError::InvalidPolicy(
            "at least one retention bucket must be kept".into(),
        ));
    }
    Ok(())
}
