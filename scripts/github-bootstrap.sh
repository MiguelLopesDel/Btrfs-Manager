#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${GITHUB_REPOSITORY:-MiguelLopesDel/Btrfs-Manager}"
REMOTE_URL="${REMOTE_URL:-git@github.com:MiguelLopesDel/Btrfs-Manager.git}"
DEFAULT_BRANCH="${DEFAULT_BRANCH:-main}"

cd "$ROOT"

if [[ -f .env && -z "${GH_TOKEN:-}" && -z "${GITHUB_TOKEN:-}" ]]; then
  while IFS= read -r line; do
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    if [[ "$line" != *=* ]]; then
      value="${line#"${line%%[![:space:]]*}"}"
      value="${value%"${value##*[![:space:]]}"}"
      if [[ -n "$value" ]]; then
        export GH_TOKEN="$value"
        break
      fi
      continue
    fi
    key="${line%%=*}"
    value="${line#*=}"
    key="${key#"${key%%[![:space:]]*}"}"
    key="${key%"${key##*[![:space:]]}"}"
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    value="${value%\"}"
    value="${value#\"}"
    value="${value%\'}"
    value="${value#\'}"
    case "$key" in
      GH_TOKEN | GITHUB_TOKEN)
        export GH_TOKEN="$value"
        break
        ;;
    esac
  done < .env
fi

export GH_TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
if [[ -z "$GH_TOKEN" ]]; then
  echo "Set GH_TOKEN or GITHUB_TOKEN in .env or the environment." >&2
  exit 1
fi

if git rev-parse --is-inside-work-tree >/dev/null 2>&1 && [[ -w .git/config ]]; then
  if git remote get-url origin >/dev/null 2>&1; then
    git remote set-url origin "$REMOTE_URL"
  else
    git remote add origin "$REMOTE_URL"
  fi
else
  echo "Skipping local origin setup; Git metadata is unavailable or read-only."
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "GitHub CLI (gh) is required." >&2
  exit 127
fi

required_checks=(
  "Rustfmt"
  "Clippy"
  "Clippy GUI"
  "Unit Tests"
  "GUI Check"
  "Loopback Btrfs Integration"
  "Dependency Security"
  "Secret Scan"
  "Coverage Ratchet"
  "Complexity And Duplication Ratchet"
  "TDD Regression Evidence"
  "Headless GUI E2E"
  "Documentation"
  "GUI Documentation"
  "Shell And Packaging"
  "Analyze"
)

checks_json="$(printf '%s\n' "${required_checks[@]}" | jq -R . | jq -s .)"

gh api \
  --method PUT \
  "repos/$REPO/branches/$DEFAULT_BRANCH/protection" \
  --input - <<JSON
{
  "required_status_checks": {
    "strict": true,
    "contexts": $(printf '%s' "$checks_json")
  },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "require_last_push_approval": true
  },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "block_creations": false,
  "required_conversation_resolution": true,
  "lock_branch": false,
  "allow_fork_syncing": true
}
JSON

echo "Configured origin and branch protection for $REPO:$DEFAULT_BRANCH."
