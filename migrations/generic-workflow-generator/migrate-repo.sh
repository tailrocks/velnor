#!/usr/bin/env bash
# Migrate a single repository to velnor-workflow generated CI.
# Usage: migrate-repo.sh owner/repo [--merge]
set -euo pipefail

export PATH="/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:${PATH:-}"
unset MISE_SHELL 2>/dev/null || true

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 owner/repo [--merge]" >&2
  exit 1
fi

REPO_SLUG="$1"
if [[ "$REPO_SLUG" =~ ^([A-Za-z0-9][A-Za-z0-9._-]*)/([A-Za-z0-9][A-Za-z0-9._-]*)$ ]]; then
  OWNER="${BASH_REMATCH[1]}"
  NAME="${BASH_REMATCH[2]}"
else
  echo "REPO_SLUG must be exactly owner/repo with safe path segments" >&2
  exit 2
fi
MERGE="${2:-}"
VELNOR_ROOT="${VELNOR_ROOT:-/Users/donbeave/Projects/tailrocks/velnor-project/velnor}"
VELNOR_BIN="${VELNOR_BIN:-$VELNOR_ROOT/target/release/velnor-workflow}"
DEFAULT_WORK_ROOT="$(cd -P /tmp && pwd -P)/velnor-migration-work"
WORK_ROOT="${WORK_ROOT:-$DEFAULT_WORK_ROOT}"
while [[ "$WORK_ROOT" != "/" && "$WORK_ROOT" == */ ]]; do
  WORK_ROOT="${WORK_ROOT%/}"
done
REPO_DIR="${REPO_DIR:-}"
BRANCH="velnor-workflow-migration-$(date +%Y%m%d)"
GENERATOR_REV="$(git -C "$VELNOR_ROOT" rev-parse HEAD)"

die() {
  echo "$*" >&2
  exit 2
}

reject_unsafe_path() {
  local label="$1"
  local path="$2"
  local remainder component current="/"

  if [[ "$path" != /* || "$path" == "/" || "$path" == *"//"* ]]; then
    die "$label must be a non-root absolute path without empty segments: $path"
  fi

  remainder="${path#/}"
  while [[ -n "$remainder" ]]; do
    component="${remainder%%/*}"
    if [[ "$component" == "." || "$component" == ".." ]]; then
      die "$label must not contain dot segments: $path"
    fi
    current="${current%/}/$component"
    if [[ -L "$current" ]]; then
      die "$label contains a symlinked path component: $current"
    fi
    if [[ "$remainder" == */* ]]; then
      remainder="${remainder#*/}"
    else
      break
    fi
  done
}

assert_workspace_paths() {
  local repo_parent repo_parent_real repo_dir_real work_root_real

  reject_unsafe_path WORK_ROOT "$WORK_ROOT"
  reject_unsafe_path REPO_DIR "$REPO_DIR"
  work_root_real="$(cd -P -- "$WORK_ROOT" && pwd -P)"
  [[ "$work_root_real" == "$WORK_ROOT" ]] || die "WORK_ROOT is not canonical: $WORK_ROOT resolves to $work_root_real"
  repo_parent="${REPO_DIR%/*}"
  repo_parent_real="$(cd -P -- "$repo_parent" && pwd -P)"
  if [[ -e "$REPO_DIR" ]]; then
    [[ -d "$REPO_DIR" ]] || die "REPO_DIR exists but is not a directory: $REPO_DIR"
    repo_dir_real="$(cd -P -- "$REPO_DIR" && pwd -P)"
  else
    repo_dir_real="$repo_parent_real/${REPO_DIR##*/}"
  fi
  [[ "$repo_dir_real" != "$work_root_real" ]] || die "REPO_DIR must be below WORK_ROOT, not equal to it"
  case "$repo_dir_real/" in
    "$work_root_real/"*) ;;
    *) die "REPO_DIR resolves outside WORK_ROOT: $repo_dir_real" ;;
  esac
}

assert_clean_checkout() {
  local status
  status="$(git -C "$REPO_DIR" status --porcelain --untracked-files=all)"
  [[ -z "$status" ]] || die "checkout has tracked or untracked changes; refusing to fetch, switch, or reset it: $REPO_DIR"
}

reject_unsafe_path WORK_ROOT "$WORK_ROOT"
/bin/mkdir -p "$WORK_ROOT"
WORK_ROOT="$(cd -P -- "$WORK_ROOT" && pwd -P)"
REPO_DIR="${REPO_DIR:-$WORK_ROOT/$OWNER/$NAME}"
while [[ "$REPO_DIR" != "/" && "$REPO_DIR" == */ ]]; do
  REPO_DIR="${REPO_DIR%/}"
done
reject_unsafe_path REPO_DIR "$REPO_DIR"
case "$REPO_DIR/" in
  "$WORK_ROOT/"*) ;;
  *) die "REPO_DIR must be inside WORK_ROOT: $REPO_DIR" ;;
esac
[[ "$REPO_DIR" != "$WORK_ROOT" ]] || die "REPO_DIR must be below WORK_ROOT, not equal to it"

REPO_PARENT="${REPO_DIR%/*}"
/bin/mkdir -p "$REPO_PARENT"
assert_workspace_paths
DEFAULT_BRANCH="$(gh api "repos/$REPO_SLUG" --jq '.default_branch')"
if ! git check-ref-format --branch "$DEFAULT_BRANCH" >/dev/null 2>&1; then
  die "GitHub returned an invalid default branch name: $DEFAULT_BRANCH"
fi
if [[ -e "$REPO_DIR" ]]; then
  [[ -d "$REPO_DIR" ]] || die "REPO_DIR exists but is not a directory: $REPO_DIR"
  [[ -d "$REPO_DIR/.git" ]] || die "REPO_DIR exists without a .git directory: $REPO_DIR"
  reject_unsafe_path "repository metadata" "$REPO_DIR/.git"
  assert_clean_checkout
  git -C "$REPO_DIR" fetch origin
else
  gh repo clone "$REPO_SLUG" "$REPO_DIR" -- --depth 1
fi
assert_workspace_paths
assert_clean_checkout
if git -C "$REPO_DIR" show-ref --verify --quiet "refs/heads/$DEFAULT_BRANCH"; then
  git -C "$REPO_DIR" switch -- "$DEFAULT_BRANCH"
else
  git -C "$REPO_DIR" switch --track -c "$DEFAULT_BRANCH" "origin/$DEFAULT_BRANCH"
fi
git -C "$REPO_DIR" merge --ff-only "origin/$DEFAULT_BRANCH"
LOCAL_HEAD="$(git -C "$REPO_DIR" rev-parse HEAD)"
REMOTE_HEAD="$(git -C "$REPO_DIR" rev-parse "origin/$DEFAULT_BRANCH")"
[[ "$LOCAL_HEAD" == "$REMOTE_HEAD" ]] || die "checkout does not match origin/$DEFAULT_BRANCH; refusing to migrate a divergent branch"
assert_clean_checkout

reject_unsafe_path ".github-gen config" "$REPO_DIR/.github-gen/velnor-workflow.toml"
mkdir -p "$REPO_DIR/.github-gen"
if [[ ! -f "$REPO_DIR/.github-gen/velnor-workflow.toml" ]]; then
  cat > "$REPO_DIR/.github-gen/velnor-workflow.toml" <<EOF
schema = 2

[generator]
repository = "$REPO_SLUG"
revision = "$GENERATOR_REV"

[workflow]
providers = ["github-hosted", "github-self-hosted", "velnor"]
automatic_providers = ["github-hosted", "github-self-hosted", "velnor"]
default_dispatch_providers = ["github-hosted", "github-self-hosted", "velnor"]
default_branch = "$DEFAULT_BRANCH"

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[workflow.selectors.github-self-hosted]
runs_on = ["bastion-scale-set"]

[workflow.selectors.velnor]
runs_on = ["self-hosted", "velnor-target-mvp"]
EOF
else
  if ! grep -Eq '^schema = 2$' "$REPO_DIR/.github-gen/velnor-workflow.toml"; then
    echo "Existing .github-gen/velnor-workflow.toml must be converted to schema 2 before migration" >&2
    exit 1
  fi
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

"$VELNOR_BIN" "$REPO_DIR" --plain --check --default-branch "$DEFAULT_BRANCH"

if git -C "$REPO_DIR" show-ref --verify --quiet "refs/heads/$BRANCH"; then
  die "migration branch already exists locally: $BRANCH"
fi
git -C "$REPO_DIR" switch -c "$BRANCH"
git -C "$REPO_DIR" add --all -- .github-gen .github
if [[ -e "$REPO_DIR/config/fleet/velnor-host.env" ]] || \
    git -C "$REPO_DIR" ls-files --error-unmatch -- config/fleet/velnor-host.env >/dev/null 2>&1; then
  git -C "$REPO_DIR" add --all -- config/fleet/velnor-host.env
fi
if git -C "$REPO_DIR" diff --cached --quiet; then
  echo "No changes for $REPO_SLUG"
  exit 0
fi

git -C "$REPO_DIR" commit -s --trailer "Co-authored-by: Codex <codex@openai.com>" -m "$(cat <<EOF
Regenerate CI with velnor-workflow.

Generate the pinned Velnor workflow surface. Existing unowned workflows remain
subject to the generator's ownership checks.

Generator revision: $GENERATOR_REV
EOF
)"

git -C "$REPO_DIR" push -u origin "$BRANCH"

PR_URL="$(gh pr create --repo "$REPO_SLUG" --head "$BRANCH" --base "$DEFAULT_BRANCH" \
  --title "Migrate CI to velnor-workflow" \
  --body "$(cat <<EOF
## Summary
- Generate GitHub Actions workflows from \`velnor-workflow\` (revision \`$GENERATOR_REV\`)
- Keep unowned workflow files for review; the generator refuses to claim them

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
