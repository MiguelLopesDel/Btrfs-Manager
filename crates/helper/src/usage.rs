use crate::validate::{validate_path, validate_relative_btrfs_path};
use crate::{CommandRunner, Helper, HelperError, HelperResponse};
use std::path::PathBuf;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn snapshot_disk_usage_impl(
        &self,
        mountpoint: PathBuf,
        subvolume_path: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&subvolume_path, "disk usage path")?;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        let target = top.join(&subvolume_path);
        let output = self.runner.run(
            "btrfs",
            &[
                "filesystem".into(),
                "du".into(),
                "-s".into(),
                "--raw".into(),
                target.display().to_string(),
            ],
        )?;
        let usage = btrfs_manager_core::parse_btrfs_filesystem_du(&output)?;
        Ok(HelperResponse {
            ok: true,
            message: format!(
                "total {} exclusive {} shared {}",
                usage.total_bytes, usage.exclusive_bytes, usage.shared_bytes
            ),
            data: Some(serde_json::to_value(usage)?),
        })
    }
}
