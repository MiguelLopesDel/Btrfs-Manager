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
}
