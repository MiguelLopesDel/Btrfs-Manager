use crate::subvolume::normalize_findmnt_source;
use crate::validate::{
    validate_mount_subvolume_option_path, validate_path, validate_relative_btrfs_path,
};
use crate::{CommandRunner, Helper, HelperError, HelperResponse};
use btrfs_manager_core::paths::validate_absolute_no_traversal;
use std::path::PathBuf;

impl<R: CommandRunner> Helper<R> {
    pub(crate) fn create_snapshot_raw(
        &self,
        source: PathBuf,
        destination: PathBuf,
        readonly: bool,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&source)?;
        validate_path(&destination)?;
        let mut args = vec!["subvolume".into(), "snapshot".into()];
        if readonly {
            args.push("-r".into());
        }
        args.push(source.display().to_string());
        args.push(destination.display().to_string());
        self.runner.run("btrfs", &args)?;
        Ok(HelperResponse {
            ok: true,
            message: "snapshot created".into(),
            data: None,
        })
    }

    pub(crate) fn delete_snapshot_raw(&self, path: PathBuf) -> Result<HelperResponse, HelperError> {
        validate_path(&path)?;
        let args = vec![
            "subvolume".into(),
            "delete".into(),
            path.display().to_string(),
        ];
        self.runner.run("btrfs", &args)?;
        Ok(HelperResponse {
            ok: true,
            message: "snapshot deleted".into(),
            data: None,
        })
    }

    pub(crate) fn set_snapshot_readonly_raw(
        &self,
        path: PathBuf,
        readonly: bool,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&path)?;
        let value = if readonly { "true" } else { "false" };
        let args = vec![
            "property".into(),
            "set".into(),
            path.display().to_string(),
            "ro".into(),
            value.into(),
        ];
        self.runner.run("btrfs", &args)?;
        Ok(HelperResponse {
            ok: true,
            message: format!("readonly set to {value}"),
            data: None,
        })
    }

    pub(crate) fn mount_snapshot_bind_readonly(
        &self,
        source: PathBuf,
        target: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&source)?;
        validate_path(&target)?;
        tracing::debug!(
            source = %source.display(),
            target = %target.display(),
            "mounting snapshot read-only"
        );
        let bind_args = vec![
            "--bind".into(),
            source.display().to_string(),
            target.display().to_string(),
        ];
        self.runner.run("mount", &bind_args)?;
        let readonly_args = vec![
            "-o".into(),
            "remount,bind,ro".into(),
            target.display().to_string(),
        ];
        self.runner.run("mount", &readonly_args)?;
        Ok(HelperResponse {
            ok: true,
            message: "snapshot mounted".into(),
            data: None,
        })
    }

    pub(crate) fn mount_top_level_impl(
        &self,
        mountpoint: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        let top = self.ensure_top_level_mount(&mountpoint)?;
        Ok(HelperResponse {
            ok: true,
            message: "top-level ready".into(),
            data: Some(serde_json::to_value(top)?),
        })
    }

    pub(crate) fn unmount_snapshot_impl(
        &self,
        target: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&target)?;
        tracing::debug!(target = %target.display(), "unmounting snapshot");
        let args = vec![target.display().to_string()];
        self.runner.run("umount", &args)?;
        Ok(HelperResponse {
            ok: true,
            message: "snapshot unmounted".into(),
            data: None,
        })
    }

    pub(crate) fn cleanup_managed_mounts_impl(&self) -> Result<HelperResponse, HelperError> {
        let count = self.cleanup_managed_mounts()?;
        Ok(HelperResponse {
            ok: true,
            message: format!("cleaned up {count} managed mount(s)"),
            data: None,
        })
    }

    pub(crate) fn mount_subvolume_impl(
        &self,
        mountpoint: PathBuf,
        subvol_path: PathBuf,
        target: PathBuf,
    ) -> Result<HelperResponse, HelperError> {
        validate_path(&mountpoint)?;
        validate_relative_btrfs_path(&subvol_path, "mount subvolume path")?;
        validate_mount_subvolume_option_path(&subvol_path)?;
        validate_path(&target)?;
        let device_output = self.runner.run(
            "findmnt",
            &[
                "-n".into(),
                "-o".into(),
                "SOURCE".into(),
                "--target".into(),
                mountpoint.display().to_string(),
            ],
        )?;
        let device = normalize_findmnt_source(device_output.trim());
        std::fs::create_dir_all(&target)?;
        let subvol_opt = format!("subvol={}", subvol_path.display());
        self.runner.run(
            "mount",
            &[
                "-t".into(),
                "btrfs".into(),
                "-o".into(),
                subvol_opt,
                device,
                target.display().to_string(),
            ],
        )?;
        tracing::info!(
            subvol = %subvol_path.display(),
            target = %target.display(),
            "subvolume mounted"
        );
        Ok(HelperResponse {
            ok: true,
            message: "subvolume mounted".into(),
            data: None,
        })
    }

    pub(crate) fn open_file_manager(
        &self,
        path: PathBuf,
        display: String,
        wayland_display: String,
        xdg_runtime_dir: String,
    ) -> Result<HelperResponse, HelperError> {
        validate_absolute_no_traversal(&path)?;
        let fm = [
            "/usr/bin/dolphin",
            "/usr/bin/nautilus",
            "/usr/bin/thunar",
            "/usr/bin/nemo",
        ]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no supported file manager found (tried dolphin, nautilus, thunar, nemo)",
            )
        })?;
        tracing::info!(path = %path.display(), fm, "opening file manager as root");
        std::process::Command::new(fm)
            .arg(&path)
            .env("DISPLAY", &display)
            .env("WAYLAND_DISPLAY", &wayland_display)
            .env("XDG_RUNTIME_DIR", &xdg_runtime_dir)
            .spawn()?;
        Ok(HelperResponse {
            ok: true,
            message: format!("file manager opened at {}", path.display()),
            data: None,
        })
    }

    pub(crate) fn cleanup_managed_mounts(&self) -> Result<usize, HelperError> {
        let mount_roots = self.managed_mount_roots();

        // List ALL current mounts and filter to managed roots.
        // Using --target with -R would find the parent mount of the root and list
        // all its submounts (potentially the entire system), so we list everything
        // and filter ourselves instead.
        let args = vec!["-n".into(), "-r".into(), "-o".into(), "TARGET".into()];
        let output = match self.runner.run("findmnt", &args) {
            Ok(o) => o,
            Err(HelperError::CommandFailed { .. }) => return Ok(0),
            Err(err) => return Err(err),
        };

        let mut targets: Vec<PathBuf> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| PathBuf::from(line.trim()))
            .filter(|path| mount_roots.iter().any(|root| path.starts_with(root)))
            .collect();

        targets.sort_by_key(|target| std::cmp::Reverse(target.as_os_str().len()));
        targets.dedup();

        let mut cleaned = 0;
        for target in targets {
            tracing::debug!(target = %target.display(), "unmounting managed browse mount");
            let args = vec![target.display().to_string()];
            self.runner.run("umount", &args)?;
            cleaned += 1;
        }

        Ok(cleaned)
    }
}
