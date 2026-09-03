//! Startup "a newer version exists" banner. The actual install is still not
//! something the GUI does on its own file-write authority — it goes through
//! the helper's `ApplySelfUpdate`, authorized by Polkit (see
//! `gui::update_apply`), so the GUI never writes to paths pacman owns
//! directly. This module only checks GitHub and, if there's a newer
//! release, remembers its download URLs for the "Atualizar" button.
//!
//! Best-effort by design: no embedded build SHA, no network, or GitHub
//! unreachable all just mean "no banner" — this must never interrupt normal
//! use of the app.

use std::cell::RefCell;
use std::rc::Rc;

use btrfs_manager_core::LatestRelease;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use super::i18n::tr;

const REPO: &str = "MiguelLopesDel/Btrfs-Manager";

/// What the "Atualizar" button needs once a newer release has been found.
pub(crate) struct PendingUpdate {
    pub(crate) package_url: String,
    pub(crate) package_name: String,
    pub(crate) checksums_url: String,
}

pub(crate) struct UpdateBanner {
    pub(crate) revealer: gtk4::Revealer,
    label: gtk4::Label,
    pub(crate) update_btn: gtk4::Button,
    pub(crate) pending: Rc<RefCell<Option<PendingUpdate>>>,
}

pub(crate) fn build_update_banner() -> UpdateBanner {
    let label = gtk4::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    let update_btn = gtk4::Button::builder()
        .label(tr("update_now"))
        .css_classes(["suggested-action"])
        .valign(gtk4::Align::Center)
        .build();
    let dismiss_btn = gtk4::Button::builder()
        .label(tr("later"))
        .css_classes(["flat"])
        .valign(gtk4::Align::Center)
        .build();

    let banner = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    banner.append(&label);
    banner.append(&update_btn);
    banner.append(&dismiss_btn);
    let frame = gtk4::Frame::builder().child(&banner).build();

    let revealer = gtk4::Revealer::builder()
        .transition_type(gtk4::RevealerTransitionType::SlideDown)
        .reveal_child(false)
        .child(&frame)
        .build();

    let revealer_for_dismiss = revealer.clone();
    dismiss_btn.connect_clicked(move |_| {
        revealer_for_dismiss.set_reveal_child(false);
    });

    UpdateBanner {
        revealer,
        label,
        update_btn,
        pending: Rc::new(RefCell::new(None)),
    }
}

/// Kicks off the (best-effort, silent-on-failure) update check in the
/// background. Call once, at startup.
pub(crate) fn check_for_update(banner: &UpdateBanner) {
    if std::env::var_os("BTRFS_MANAGER_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let build_sha = env!("BM_BUILD_GIT_SHA");
    if build_sha.is_empty() {
        return;
    }

    let revealer = banner.revealer.clone();
    let label = banner.label.clone();
    let pending = banner.pending.clone();
    let build_sha = build_sha.to_string();
    glib::MainContext::default().spawn_local(async move {
        let Ok(Some((release, behind_by))) =
            gio::spawn_blocking(move || fetch_release_and_status(&build_sha)).await
        else {
            return;
        };
        if behind_by == 0 {
            return;
        }
        let (Some(package), Some(checksums)) = (release.package_asset(), release.checksums_asset())
        else {
            // Release exists but doesn't have the assets we expect yet
            // (e.g. the workflow is still running) — nothing to offer.
            return;
        };
        *pending.borrow_mut() = Some(PendingUpdate {
            package_url: package.download_url.clone(),
            package_name: package.name.clone(),
            checksums_url: checksums.download_url.clone(),
        });
        label.set_text(&format!(
            "Nova versão disponível ({behind_by} commit(s) à frente) — clique em Atualizar"
        ));
        revealer.set_reveal_child(true);
    });
}

/// Blocking: fetches the latest release, then how far behind it the running
/// build is. `None` on any failure (network, parse, rate limit) —
/// deliberately swallowed, never surfaced to the user as an error.
fn fetch_release_and_status(build_sha: &str) -> Option<(LatestRelease, u32)> {
    let agent = github_agent();
    let release_url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut response = agent
        .get(&release_url)
        .header("User-Agent", "btrfs-manager-update-check")
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    let release = btrfs_manager_core::parse_latest_release(&body).ok()?;

    let compare_url = format!(
        "https://api.github.com/repos/{REPO}/compare/{build_sha}...{}",
        release.tag_name
    );
    let mut response = agent
        .get(&compare_url)
        .header("User-Agent", "btrfs-manager-update-check")
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    let status = btrfs_manager_core::parse_compare_response(&body, &release.tag_name).ok()?;

    Some((release, status.behind_by))
}

/// TLS verified via the OS's own trust store (see Cargo.toml comment on the
/// ureq dependency) rather than ureq's default bundled Mozilla list. Shared
/// with `update_apply.rs`, which downloads the actual package over the same
/// kind of connection.
pub(crate) fn github_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}
