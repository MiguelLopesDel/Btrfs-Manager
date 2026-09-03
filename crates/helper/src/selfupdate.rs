use crate::validate::validate_update_package_path;
use crate::{CommandRunner, Helper, HelperError, HelperResponse};
use std::path::PathBuf;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn apply_self_update_impl(
        &self,
        package_path: PathBuf,
        expected_sha256: String,
    ) -> Result<HelperResponse, HelperError> {
        // Only reachable via authenticated D-Bus (the CLI/scheduled path never
        // sets caller_uid) — self-update is far too sensitive to accept from
        // an unauthenticated local invocation.
        if self.caller_uid.is_none() {
            return Err(HelperError::InvalidPolicy(
                "self-update requires an authenticated caller".into(),
            ));
        }
        validate_update_package_path(&package_path, &self.managed_mount_roots())?;

        let bytes = std::fs::read(&package_path)?;
        let actual_sha256 = btrfs_manager_core::sha256_hex(&bytes);
        if actual_sha256 != expected_sha256 {
            return Err(HelperError::InvalidPolicy(format!(
                "update package checksum mismatch for {}",
                package_path.display()
            )));
        }

        self.runner.run(
            "pacman",
            &[
                "-U".into(),
                "--noconfirm".into(),
                package_path.display().to_string(),
            ],
        )?;

        tracing::info!(path = %package_path.display(), "self-update package installed");
        Ok(HelperResponse {
            ok: true,
            message: "update installed — restart the app to use the new version".into(),
            data: None,
        })
    }
}
