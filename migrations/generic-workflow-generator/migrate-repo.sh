#!/usr/bin/env bash
# Migrate a single repository to velnor-workflow generated CI.
# Usage: migrate-repo.sh owner/repo [--merge]
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 owner/repo [--merge]" >&2
  exit 1
fi

REPO_SLUG="$1"
MERGE="${2:-}"
OWNER="${REPO_SLUG%%/*}"
NAME="${REPO_SLUG##*/}"
VELNOR_ROOT="${VELNOR_ROOT:-/Users/donbeave/Projects/tailrocks/velnor-project/velnor}"
VELNOR_BIN="${VELNOR_BIN:-$VELNOR_ROOT/target/release/velnor-workflow}"
WORK_ROOT="${WORK_ROOT:-/tmp/velnor-migration-work}"
REPO_DIR="$WORK_ROOT/$OWNER/$NAME"
BRANCH="velnor-workflow-migration-$(date +%Y%m%d)"
GENERATOR_REV="$(git -C "$VELNOR_ROOT" rev-parse HEAD)"

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

mkdir -p "$REPO_DIR/.github-gen"
if [[ ! -f "$REPO_DIR/.github-gen/velnor-workflow.toml" ]]; then
  cat > "$REPO_DIR/.github-gen/velnor-workflow.toml" <<EOF
schema = 1

[generator]
repository = "$REPO_SLUG"
revision = "$GENERATOR_REV"

[workflow]
runners = "both"
automatic = "both"
default_dispatch_runner = "github"
automatic_lanes = "github"
github_runner = "ubuntu-24.04"
velnor_labels = ["self-hosted", "velnor-target-mvp"]
default_branch = "$DEFAULT_BRANCH"
EOF
else
  # Ensure revision pin exists and is current.
  if grep -q '^revision = ' "$REPO_DIR/.github-gen/velnor-workflow.toml"; then
    sed -i '' "s/^revision = .*/revision = \"$GENERATOR_REV\"/" "$REPO_DIR/.github-gen/velnor-workflow.toml"
  else
    sed -i '' "/^\[generator\]/a\\
revision = \"$GENERATOR_REV\"
" "$REPO_DIR/.github-gen/velnor-workflow.toml"
  fi
fi

"$VELNOR_BIN" "$REPO_DIR" --plain --force --default-branch "$DEFAULT_BRANCH"

# Remove legacy velnor-actions-generator workflows if generator replaced them.
for legacy in ci.yml release.yml renovate.yml package-update.yml publish.yml; do
  if [[ -f "$REPO_DIR/.github/workflows/$legacy" ]]; then
    if grep -q "velnor-actions-generator\|velnor-actions/.github/workflows" "$REPO_DIR/.github/workflows/$legacy" 2>/dev/null; then
      rm -f "$REPO_DIR/.github/workflows/$legacy"
    fi
  fi
done

"$VELNOR_BIN" "$REPO_DIR" --plain --check --default-branch "$DEFAULT_BRANCH"

git -C "$REPO_DIR" checkout -B "$BRANCH"
git -C "$REPO_DIR" add -A
if git -C "$REPO_DIR" diff --cached --quiet; then
  echo "No changes for $REPO_SLUG"
  exit 0
fi

git -C "$REPO_DIR" commit -m "$(cat <<EOF
Migrate CI to velnor-workflow generated workflows.

Replace legacy velnor-actions-generator/manual workflows with the latest
velnor-workflow surface. GitHub-hosted runners remain the default automatic
lane; Velnor runners are available via dispatch.

Generator revision: $GENERATOR_REV
EOF
)"

git -C "$REPO_DIR" push -u origin "$BRANCH" --force

PR_URL="$(gh pr create --repo "$REPO_SLUG" --head "$BRANCH" --base "$DEFAULT_BRANCH" \
  --title "Migrate CI to velnor-workflow" \
  --body "$(cat <<EOF
## Summary
- Generate GitHub Actions workflows from \`velnor-workflow\` (revision \`$GENERATOR_REV\`)
- Remove legacy \`velnor-actions-generator\` / \`velnor-actions\` workflow dependencies
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
