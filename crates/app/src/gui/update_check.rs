//! Startup "a newer version exists" banner. Unlike a raw git-checkout app,
//! this one installs as a pacman package (see packaging/arch/PKGBUILD) — the
//! GUI must never overwrite files pacman owns, so this only checks and
//! notifies. Applying the update is left to the user's terminal, same as
//! every other pacman package.
//!
//! Best-effort by design: no embedded build SHA, no network, or GitHub
//! unreachable all just mean "no banner" — this must never interrupt normal
//! use of the app.

use btrfs_manager_core::UpdateStatus;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use super::i18n::tr;

const REPO: &str = "MiguelLopesDel/Btrfs-Manager";
const BRANCH: &str = "main";
// Not on the real AUR yet (see docs/product-roadmap.md, Fase 10) — once it is,
// this becomes something like "yay -Syu btrfs-manager-git".
const UPDATE_COMMAND: &str = "bash scripts/pkg-install.sh";

pub(crate) struct UpdateBanner {
    pub(crate) revealer: gtk4::Revealer,
    label: gtk4::Label,
}

pub(crate) fn build_update_banner() -> UpdateBanner {
    let label = gtk4::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    let copy_btn = gtk4::Button::builder()
        .label(tr("copy_update_command"))
        .css_classes(["suggested-action"])
        .valign(gtk4::Align::Center)
        .build();
    copy_btn.connect_clicked(|btn| {
        btn.display().clipboard().set_text(UPDATE_COMMAND);
    });
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
    banner.append(&copy_btn);
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

    UpdateBanner { revealer, label }
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
    let build_sha = build_sha.to_string();
    glib::MainContext::default().spawn_local(async move {
        let Ok(Some(status)) = gio::spawn_blocking(move || fetch_status(&build_sha)).await else {
            return;
        };
        if status.behind_by == 0 {
            return;
        }
        label.set_text(&format!(
            "Nova versão disponível ({} commit(s) à frente) — atualize com o comando abaixo",
            status.behind_by
        ));
        revealer.set_reveal_child(true);
    });
}

/// Blocking: one HTTPS GET to GitHub's compare API. `None` on any failure
/// (network, parse, rate limit) — deliberately swallowed, never surfaced to
/// the user as an error.
fn fetch_status(build_sha: &str) -> Option<UpdateStatus> {
    let url = format!("https://api.github.com/repos/{REPO}/compare/{build_sha}...{BRANCH}");
    // Verify TLS certs via the OS's own trust store (see Cargo.toml comment on
    // the ureq dependency) rather than ureq's default bundled Mozilla list.
    let agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent();
    let mut response = agent
        .get(&url)
        .header("User-Agent", "btrfs-manager-update-check")
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?;
    let body = response.body_mut().read_to_string().ok()?;
    btrfs_manager_core::parse_compare_response(&body, BRANCH).ok()
}
