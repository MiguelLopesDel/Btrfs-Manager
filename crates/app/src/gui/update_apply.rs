//! Wires the update banner's "Atualizar" button: download the release
//! package, verify it against the published checksum, hand it to the
//! helper (which re-verifies independently and runs `pacman -U` under a
//! Polkit prompt), then offer to restart into the new binary.

use std::path::PathBuf;

use btrfs_manager_helper::HelperRequest;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use super::errors::show_toast;
use super::helper_client::handle_privileged_async;
use super::i18n::tr;
use super::state::UiState;
use super::update_check::{PendingUpdate, UpdateBanner, github_agent};

pub(crate) fn wire_update_banner(ui_state: &UiState, banner: &UpdateBanner) {
    let state = ui_state.clone();
    let pending = banner.pending.clone();
    banner.update_btn.connect_clicked(move |btn| {
        let Some(update) = pending.borrow_mut().take() else {
            return;
        };
        let state = state.clone();
        let btn = btn.clone();
        btn.set_sensitive(false);
        state.spinner.start();
        glib::MainContext::default().spawn_local(async move {
            let result = apply_update(update).await;
            state.spinner.stop();
            btn.set_sensitive(true);
            match result {
                Ok(()) => offer_restart(&btn),
                Err(err) => show_toast(&state.toast_overlay, &tr_error(&err)),
            }
        });
    });
}

async fn apply_update(update: PendingUpdate) -> anyhow::Result<()> {
    let package_path = gio::spawn_blocking(move || download_and_verify(&update))
        .await
        .map_err(|_| anyhow::anyhow!("download thread panicked"))??;
    let expected_sha256 = std::fs::read(&package_path)
        .map(|bytes| btrfs_manager_core::sha256_hex(&bytes))
        .map_err(anyhow::Error::from)?;
    handle_privileged_async(HelperRequest::ApplySelfUpdate {
        package_path,
        expected_sha256,
    })
    .await?;
    Ok(())
}

/// Blocking: downloads the package + checksums, verifies the package's
/// hash matches before writing anything to disk, and saves it to the same
/// `/run/user/<uid>/btrfs-manager/update/` directory the helper expects
/// (see `crates/helper/src/validate.rs::validate_update_package_path`).
fn download_and_verify(update: &PendingUpdate) -> anyhow::Result<PathBuf> {
    let agent = github_agent();
    let package_bytes = fetch_bytes(&agent, &update.package_url)?;
    let checksums_text = String::from_utf8(fetch_bytes(&agent, &update.checksums_url)?)?;
    let expected = btrfs_manager_core::find_checksum(&checksums_text, &update.package_name)
        .ok_or_else(|| anyhow::anyhow!("checksum for {} not published", update.package_name))?;
    let actual = btrfs_manager_core::sha256_hex(&package_bytes);
    if actual != expected {
        anyhow::bail!("downloaded package checksum does not match the published one");
    }

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is not set"))?;
    let dir = PathBuf::from(runtime_dir).join("btrfs-manager/update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&update.package_name);
    std::fs::write(&path, &package_bytes)?;
    Ok(path)
}

fn fetch_bytes(agent: &ureq::Agent, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut response = agent
        .get(url)
        .header("User-Agent", "btrfs-manager-update-check")
        .call()?;
    Ok(response.body_mut().read_to_vec()?)
}

fn tr_error(err: &anyhow::Error) -> String {
    format!("{}: {err}", tr("update_now"))
}

fn offer_restart(parent: &gtk4::Button) {
    let dialog = libadwaita::AlertDialog::builder()
        .heading("Atualizado")
        .body("A nova versão foi instalada. Reiniciar agora para usá-la?")
        .build();
    dialog.add_response("later", tr("later"));
    dialog.add_response("restart", "Reiniciar agora");
    dialog.set_response_appearance("restart", libadwaita::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("restart"));
    dialog.connect_response(None, |_, response| {
        if response == "restart" {
            restart();
        }
    });
    let window = parent.root().and_downcast::<gtk4::Window>();
    dialog.present(window.as_ref());
}

/// Replaces this process image with a fresh launch of the (now updated)
/// binary — same idea as klipe's `os.execv`, so "restart" is really one
/// click, not "close this window and remember to relaunch yourself."
fn restart() -> ! {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("btrfs-manager-app"));
    let err = std::process::Command::new(exe).exec();
    panic!("failed to restart into the updated binary: {err}");
}
