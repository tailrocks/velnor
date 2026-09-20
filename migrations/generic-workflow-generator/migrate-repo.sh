#!/usr/bin/env bash
# Migrate a single repository to velnor-workflow generated CI.
# Usage: migrate-repo.sh owner/repo [--merge]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/schema2-config.sh"

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "Usage: $0 owner/repo [--merge]" >&2
  exit 1
fi

REPO_SLUG="$1"
MERGE="${2:-}"
if [[ -n "$MERGE" && "$MERGE" != "--merge" ]]; then
  echo "Unknown option: $MERGE" >&2
  echo "Usage: $0 owner/repo [--merge]" >&2
  exit 1
fi
if [[ ! "$REPO_SLUG" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; then
  echo "Repository must be a GitHub owner/repo slug: $REPO_SLUG" >&2
  exit 1
fi

OWNER="${REPO_SLUG%%/*}"
NAME="${REPO_SLUG##*/}"
if [[ "$OWNER" == .* || "$NAME" == .* || "$NAME" == *.[gG][iI][tT] ]]; then
  echo "Repository must use valid GitHub owner and repository names: $REPO_SLUG" >&2
  exit 1
fi

VELNOR_ROOT="${VELNOR_ROOT:-/Users/donbeave/Projects/tailrocks/velnor-project/velnor}"
VELNOR_BIN="${VELNOR_BIN:-$VELNOR_ROOT/target/release/velnor-workflow}"
WORK_ROOT="${WORK_ROOT:-/tmp/velnor-migration-work}"
REPO_DIR="$WORK_ROOT/$OWNER/$NAME"
BRANCH="velnor-workflow-migration-$(date +%Y%m%d)"
if [[ "$VELNOR_BIN" != /* ]]; then
  VELNOR_BIN="$PWD/$VELNOR_BIN"
fi

if ! GENERATOR_REV="$(git -C "$VELNOR_ROOT" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)"; then
  echo "Cannot resolve a generator commit in $VELNOR_ROOT" >&2
  exit 1
fi
if ! GENERATOR_CLOSURE="$(require_matching_generator_binary "$GENERATOR_REV" "$VELNOR_ROOT" "$VELNOR_BIN")"; then
  exit 1
fi

mkdir -p "$WORK_ROOT/$OWNER"
if [[ -d "$REPO_DIR/.git" ]]; then
  git -C "$REPO_DIR" fetch origin
  git -C "$REPO_DIR" checkout main 2>/dev/null || git -C "$REPO_DIR" checkout master
  git -C "$REPO_DIR" reset --hard "origin/$(git -C "$REPO_DIR" rev-parse --abbrev-ref HEAD)"
else
  gh repo clone "$REPO_SLUG" "$REPO_DIR" -- --depth 1
fi

DEFAULT_BRANCH="$(gh api "repos/$REPO_SLUG" --jq '.default_branch')"
git -C "$REPO_DIR" checkout "$DEFAULT_BRANCH"
git -C "$REPO_DIR" pull --ff-only origin "$DEFAULT_BRANCH" || true

# Convert and render in an isolated copy. The target worktree remains untouched
# until the config is structurally valid and the pinned renderer proves it can
# reproduce the generated tree byte-for-byte.
STAGING_DIR=""
cleanup_staging() {
  if [[ -n "$STAGING_DIR" && -d "$STAGING_DIR" ]]; then
    rm -rf -- "$STAGING_DIR"
  fi
}
trap cleanup_staging EXIT
STAGING_DIR="$(mktemp -d "$WORK_ROOT/.velnor-migration-stage.XXXXXX")"
cp -a "$REPO_DIR/." "$STAGING_DIR/"

ensure_schema2_generation_config \
  "$STAGING_DIR/.github-gen/velnor-workflow.toml" \
  "$REPO_SLUG" \
  "$GENERATOR_REV" \
  "$DEFAULT_BRANCH"

"$VELNOR_BIN" "$STAGING_DIR" --plain --force --default-branch "$DEFAULT_BRANCH"

# Remove legacy velnor-actions-generator workflows if generator replaced them.
for legacy in ci.yml release.yml renovate.yml package-update.yml publish.yml; do
  if [[ -f "$STAGING_DIR/.github/workflows/$legacy" ]]; then
    if grep -q "velnor-actions-generator\|velnor-actions/.github/workflows" "$STAGING_DIR/.github/workflows/$legacy" 2>/dev/null; then
      rm -f "$STAGING_DIR/.github/workflows/$legacy"
    fi
  fi
done

"$VELNOR_BIN" "$STAGING_DIR" --plain --check --default-branch "$DEFAULT_BRANCH"
VELNOR_WORKFLOW_PINNED_BINARY="$VELNOR_BIN" \
VELNOR_WORKFLOW_PINNED_CLOSURE="$GENERATOR_CLOSURE" \
  "$VELNOR_BIN" "$STAGING_DIR" --plain --verify-pinned --default-branch "$DEFAULT_BRANCH"

if ! command -v rsync >/dev/null 2>&1; then
  echo "Cannot apply verified migration: rsync is required to preserve the checkout while syncing the staged result" >&2
  exit 1
fi
rsync -a --delete --exclude='/.git/' "$STAGING_DIR/" "$REPO_DIR/"

git -C "$REPO_DIR" checkout -B "$BRANCH"
git -C "$REPO_DIR" add -A
if git -C "$REPO_DIR" diff --cached --quiet; then
  echo "No changes for $REPO_SLUG"
  exit 0
fi

git -C "$REPO_DIR" commit -s -m "$(cat <<EOF
Migrate CI to velnor-workflow generated workflows.

Replace legacy velnor-actions-generator/manual workflows with the latest
velnor-workflow surface. GitHub-hosted runners remain the default automatic
lane; Velnor runners are available via dispatch.

Generator revision: $GENERATOR_REV
Co-authored-by: Codex <codex@openai.com>
EOF
)"

git -C "$REPO_DIR" push -u origin "$BRANCH" --force

PR_URL="$(gh pr create --repo "$REPO_SLUG" --head "$BRANCH" --base "$DEFAULT_BRANCH" \
  --title "Migrate CI to velnor-workflow" \
  --body "$(cat <<EOF
## Summary
- Generate GitHub Actions workflows from velnor-workflow revision $GENERATOR_REV
- Remove legacy velnor-actions-generator / velnor-actions workflow dependencies
- GitHub-hosted runners remain the default automatic lane

## Test plan
- [ ] Review generated workflow diff
- [ ] Merge migration PR
- [ ] Fix any post-merge CI failures on default branch
EOF
)" 2>/dev/null || gh pr list --repo "$REPO_SLUG" --head "$BRANCH" --json url -q '.[0].url')"

echo "PR: $PR_URL"

if [[ "$MERGE" == "--merge" ]]; then
  gh pr merge "$PR_URL" --merge --admin --delete-branch || gh pr merge "$PR_URL" --merge --delete-branch
  echo "Merged: $PR_URL"
fi
