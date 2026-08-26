//! Embeds the build's git commit SHA so the running binary can ask GitHub
//! how far behind `main` it is (see gui::update_check). Empty when built
//! outside a git checkout (e.g. a source tarball with no `.git`) — the
//! update check degrades to a no-op in that case rather than failing.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=BM_BUILD_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
