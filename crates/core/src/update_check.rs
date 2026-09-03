//! Pure parsing for GitHub's "compare two commits" API response. No network
//! I/O here — the actual HTTP call lives in the GUI (crates/app), which is
//! the only layer that needs to reach the network for this.

use crate::parser::ParseError;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateStatus {
    pub behind_by: u32,
    pub latest_sha: String,
}

#[derive(Deserialize)]
struct CompareResponse {
    // NOTE: for `GET /compare/{base}...{head}`, GitHub's `ahead_by` is how
    // far `head` is ahead of `base` — i.e. exactly "how far behind is my
    // build" when `base` is our embedded commit and `head` is `main`. Their
    // `behind_by` means the opposite (base has commits head doesn't, e.g. a
    // diverged/rebased build) and isn't what we want here. Confirmed against
    // the real API, not just the docs — easy field to get backwards.
    ahead_by: u32,
    commits: Vec<CommitEntry>,
}

#[derive(Deserialize)]
struct CommitEntry {
    sha: String,
}

/// Parses the JSON body of
/// `GET /repos/{owner}/{repo}/compare/{base}...{head}`. `latest_sha` is the
/// tip of `head` (the last entry in `commits`), or `head_ref_hint` when
/// there's nothing new (`behind_by == 0`, `commits` empty).
pub fn parse_compare_response(json: &str, head_ref_hint: &str) -> Result<UpdateStatus, ParseError> {
    let response: CompareResponse =
        serde_json::from_str(json).map_err(|_| ParseError::InvalidField {
            field: "compare response",
            value: json.chars().take(80).collect(),
        })?;
    let latest_sha = response
        .commits
        .last()
        .map(|commit| commit.sha.clone())
        .unwrap_or_else(|| head_ref_hint.to_string());
    Ok(UpdateStatus {
        behind_by: response.ahead_by,
        latest_sha,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

impl LatestRelease {
    /// The installable package asset: name starts with `btrfs-manager-git-`
    /// and ends with `.pkg.tar.zst` — matches what `.github/workflows/release.yml`
    /// actually publishes, not just any attached file.
    pub fn package_asset(&self) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|asset| {
            asset.name.starts_with("btrfs-manager-git-") && asset.name.ends_with(".pkg.tar.zst")
        })
    }

    pub fn checksums_asset(&self) -> Option<&ReleaseAsset> {
        self.assets
            .iter()
            .find(|asset| asset.name == "checksums.txt")
    }
}

#[derive(Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    assets: Vec<AssetEntry>,
}

#[derive(Deserialize)]
struct AssetEntry {
    name: String,
    browser_download_url: String,
}

/// Parses the JSON body of `GET /repos/{owner}/{repo}/releases/latest`.
pub fn parse_latest_release(json: &str) -> Result<LatestRelease, ParseError> {
    let response: ReleaseResponse =
        serde_json::from_str(json).map_err(|_| ParseError::InvalidField {
            field: "latest release response",
            value: json.chars().take(80).collect(),
        })?;
    Ok(LatestRelease {
        tag_name: response.tag_name,
        assets: response
            .assets
            .into_iter()
            .map(|asset| ReleaseAsset {
                name: asset.name,
                download_url: asset.browser_download_url,
            })
            .collect(),
    })
}

/// Finds `<filename>`'s expected hash in a `checksums.txt` body
/// (`sha256sum` output: `<hex>␠␠<filename>` per line).
pub fn find_checksum(checksums_text: &str, filename: &str) -> Option<String> {
    checksums_text.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        (name.trim() == filename).then(|| hash.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ahead_by_as_behind_by_and_latest_commit_sha() {
        // Real shape from GitHub, trimmed to the fields we read.
        let json = r#"{
            "status": "ahead",
            "ahead_by": 3,
            "behind_by": 0,
            "commits": [
                {"sha": "aaa111"},
                {"sha": "bbb222"},
                {"sha": "ccc333"}
            ]
        }"#;
        let status = parse_compare_response(json, "unused").unwrap();
        assert_eq!(status.behind_by, 3);
        assert_eq!(status.latest_sha, "ccc333");
    }

    #[test]
    fn falls_back_to_hint_when_up_to_date() {
        let json = r#"{"status": "identical", "ahead_by": 0, "behind_by": 0, "commits": []}"#;
        let status = parse_compare_response(json, "main").unwrap();
        assert_eq!(status.behind_by, 0);
        assert_eq!(status.latest_sha, "main");
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_compare_response("not json", "main").is_err());
    }

    #[test]
    fn parses_latest_release_and_finds_the_package_and_checksums_assets() {
        let json = r#"{
            "tag_name": "v1.2.3",
            "assets": [
                {"name": "btrfs-manager-git-0.1.0.r10.gabc-1-x86_64.pkg.tar.zst",
                 "browser_download_url": "https://example.com/pkg.zst"},
                {"name": "checksums.txt",
                 "browser_download_url": "https://example.com/checksums.txt"},
                {"name": "unrelated.txt",
                 "browser_download_url": "https://example.com/unrelated.txt"}
            ]
        }"#;
        let release = parse_latest_release(json).unwrap();
        assert_eq!(release.tag_name, "v1.2.3");
        assert_eq!(
            release.package_asset().unwrap().name,
            "btrfs-manager-git-0.1.0.r10.gabc-1-x86_64.pkg.tar.zst"
        );
        assert_eq!(release.checksums_asset().unwrap().name, "checksums.txt");
    }

    #[test]
    fn missing_package_asset_returns_none() {
        let json = r#"{"tag_name": "v1.0.0", "assets": []}"#;
        let release = parse_latest_release(json).unwrap();
        assert!(release.package_asset().is_none());
        assert!(release.checksums_asset().is_none());
    }

    #[test]
    fn finds_checksum_for_the_named_file_among_several_lines() {
        let checksums = "aaaa111  other-file.tar.zst\nbbbb222  btrfs-manager-git-1.pkg.tar.zst\n";
        assert_eq!(
            find_checksum(checksums, "btrfs-manager-git-1.pkg.tar.zst"),
            Some("bbbb222".to_string())
        );
        assert_eq!(find_checksum(checksums, "not-present.pkg.tar.zst"), None);
    }
}
