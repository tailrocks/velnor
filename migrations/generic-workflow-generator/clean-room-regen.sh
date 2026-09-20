#!/usr/bin/env bash
# Clean-room workflow regeneration: retire owned legacy inputs, regenerate from velnor.
# Usage: clean-room-regen.sh owner/repo [--merge] [--keep-config]
set -euo pipefail

export PATH="/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:${PATH:-}"
unset MISE_SHELL 2>/dev/null || true

REPO_SLUG="${1:?owner/repo required}"
if [[ "$REPO_SLUG" =~ ^([A-Za-z0-9][A-Za-z0-9._-]*)/([A-Za-z0-9][A-Za-z0-9._-]*)$ ]]; then
  OWNER="${BASH_REMATCH[1]}"
  NAME="${BASH_REMATCH[2]}"
else
  echo "REPO_SLUG must be exactly owner/repo with safe path segments" >&2
  exit 2
fi
MERGE="${2:-}"
KEEP_CONFIG="${3:-}"
VELNOR_ROOT="${VELNOR_ROOT:-/Users/donbeave/Projects/tailrocks/velnor-project/velnor}"
VELNOR_BIN="${VELNOR_BIN:-$VELNOR_ROOT/target/release/velnor-workflow}"
DEFAULT_WORK_ROOT="$(cd -P /tmp && pwd -P)/velnor-clean-room"
WORK_ROOT="${WORK_ROOT:-$DEFAULT_WORK_ROOT}"
while [[ "$WORK_ROOT" != "/" && "$WORK_ROOT" == */ ]]; do
  WORK_ROOT="${WORK_ROOT%/}"
done
REPO_DIR="${REPO_DIR:-$WORK_ROOT/$OWNER/$NAME}"
while [[ "$REPO_DIR" != "/" && "$REPO_DIR" == */ ]]; do
  REPO_DIR="${REPO_DIR%/}"
done
BRANCH="${BRANCH:-velnor-legacy-debt-$(date +%Y%m%d)}"
GENERATOR_REV="${GENERATOR_REV:-1279c4f92c97b75dc4cc627f122e119f8a5eae16}"

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
  local repo_parent repo_parent_real repo_dir_real

  reject_unsafe_path WORK_ROOT "$WORK_ROOT"
  reject_unsafe_path REPO_DIR "$REPO_DIR"
  WORK_ROOT_REAL="$(cd -P -- "$WORK_ROOT" && pwd -P)"
  repo_parent="${REPO_DIR%/*}"
  repo_parent_real="$(cd -P -- "$repo_parent" && pwd -P)"
  if [[ -e "$REPO_DIR" ]]; then
    [[ -d "$REPO_DIR" ]] || die "REPO_DIR exists but is not a directory: $REPO_DIR"
    repo_dir_real="$(cd -P -- "$REPO_DIR" && pwd -P)"
  else
    repo_dir_real="$repo_parent_real/${REPO_DIR##*/}"
  fi
  [[ "$repo_dir_real" != "$WORK_ROOT_REAL" ]] || die "REPO_DIR must be below WORK_ROOT, not equal to it"
  case "$repo_dir_real/" in
    "$WORK_ROOT_REAL/"*) ;;
    *) die "REPO_DIR resolves outside WORK_ROOT: $repo_dir_real" ;;
  esac
}

assert_clean_checkout() {
  local status
  status="$(/usr/bin/git -C "$REPO_DIR" status --porcelain --untracked-files=all)"
  [[ -z "$status" ]] || die "checkout has tracked or untracked changes; refusing to fetch, switch, or reset it: $REPO_DIR"
}

verify_static_file_ownership() {
  local python_bin candidate
  for candidate in "${PYTHON3:-}" /opt/homebrew/bin/python3 /opt/homebrew/bin/python3.[0-9]* \
      /usr/local/bin/python3 /usr/local/bin/python3.[0-9]* /opt/local/bin/python3 /usr/bin/python3 \
      "$(command -v python3 2>/dev/null || true)"; do
    [[ -n "$candidate" ]] || continue
    if [[ ! -x "$candidate" ]]; then
      candidate="$(command -v "$candidate" 2>/dev/null || true)"
    fi
    if [[ -n "$candidate" && -x "$candidate" ]] && "$candidate" -c 'import tomllib' >/dev/null 2>&1; then
      python_bin="$candidate"
      break
    fi
  done
  [[ -n "${python_bin:-}" ]] || die "Python 3.11+ with tomllib is required to verify static-file ownership before cleanup"

  "$python_bin" - "$REPO_DIR" "$REPO_DIR/.github-gen/velnor-workflow.toml" "$OWNERSHIP_STATE" <<'PY'
import re
import stat
import sys
from pathlib import Path, PurePosixPath

try:
    import tomllib
except ImportError as error:
    raise SystemExit(f"python3 with tomllib is required for ownership preflight: {error}")

root = Path(sys.argv[1])
config_path = Path(sys.argv[2])
state_path = Path(sys.argv[3])

def fail(message):
    raise SystemExit(f"static-file ownership preflight failed: {message}")

def no_symlink_components(path):
    try:
        relative = path.relative_to(root)
    except ValueError:
        fail(f"path is outside the checkout: {path}")
    current = root
    for component in relative.parts:
        current = current / component
        try:
            mode = current.lstat().st_mode
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(mode):
            fail(f"symlinked path component: {current}")

def safe_relative(value, *, output=False):
    path = PurePosixPath(value)
    if (
        not value
        or path.is_absolute()
        or "\\" in value
        or ":" in value
        or any(part in ("", ".", "..") for part in value.split("/"))
        or any(ord(char) < 32 or ord(char) == 127 for char in value)
    ):
        fail(f"unsafe repository path: {value!r}")
    if output and not (value.startswith(".github/") or value == "config/fleet/velnor-host.env"):
        fail(f"ownership ledger contains an unsupported output path: {value!r}")
    return path

static_files = []
if config_path.exists():
    no_symlink_components(config_path)
    try:
        config = tomllib.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot parse {config_path}: {error}")
    rows = config.get("static_files", [])
    if not isinstance(rows, list):
        fail("`static_files` must be an array of tables")
    for index, row in enumerate(rows, start=1):
        if not isinstance(row, dict) or not isinstance(row.get("file"), str):
            fail(f"static_files row {index} has no string `file` destination")
        destination = row["file"]
        safe_relative(destination)
        if not destination.startswith(".github/"):
            fail(f"static_files destination must be inside .github/: {destination!r}")
        if destination in static_files:
            fail(f"duplicate static_files destination: {destination}")
        static_files.append(destination)

state_exists = state_path.exists() or state_path.is_symlink()
if not state_exists:
    if static_files:
        fail(f"schema-2 ownership ledger is missing: {state_path}")
    raise SystemExit(0)
no_symlink_components(state_path)
try:
    state_lines = state_path.read_text(encoding="utf-8").splitlines()
except (OSError, UnicodeError) as error:
    fail(f"cannot read {state_path}: {error}")
if len(state_lines) < 7 or state_lines[0] != "# Generated ownership state; do not edit." or state_lines[1] != "schema = 2":
    fail(f"ownership ledger is not valid schema 2: {state_path}")
if state_lines[2] != "[inputs]":
    fail(f"ownership ledger has no [inputs] section: {state_path}")
for index, name in enumerate(("config", "scan"), start=3):
    fields = state_lines[index].split("\t", 1)
    if len(fields) != 2 or fields[0] != name or not re.fullmatch(r"[0-9a-fA-F]{1,16}", fields[1]):
        fail(f"ownership ledger has invalid {name} input row: {state_path}")
generator_fields = state_lines[5].split("\t", 1)
if len(generator_fields) != 2 or generator_fields[0] != "generator" or not generator_fields[1]:
    fail(f"ownership ledger has invalid generator input row: {state_path}")
if state_lines[6] != "[outputs]":
    fail(f"ownership ledger has no [outputs] section: {state_path}")

outputs = {}
for line in state_lines[7:]:
    fields = line.split("\t", 1)
    if len(fields) != 2 or not re.fullmatch(r"[0-9a-fA-F]{1,16}", fields[1]):
        fail(f"ownership ledger has malformed output row: {line!r}")
    path, raw_digest = fields
    safe_relative(path, output=True)
    if path in outputs:
        fail(f"ownership ledger repeats output row: {path}")
    outputs[path] = int(raw_digest, 16)

for destination in static_files:
    expected = outputs.get(destination)
    if expected is None:
        fail(f"ownership ledger has no exact output row for {destination}")
    output_path = root / destination
    no_symlink_components(output_path)
    try:
        content = output_path.read_bytes()
    except OSError as error:
        fail(f"cannot read declared output {destination}: {error}")
    digest = 0xCBF29CE484222325
    for byte in content:
        digest = ((digest ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    if digest != expected:
        fail(f"declared output digest does not match schema-2 ownership row: {destination}")
PY
}

reject_unsafe_path WORK_ROOT "$WORK_ROOT"
reject_unsafe_path REPO_DIR "$REPO_DIR"
case "$REPO_DIR/" in
  "$WORK_ROOT/"*) ;;
  *) die "REPO_DIR must be inside WORK_ROOT: $REPO_DIR" ;;
esac
[[ "$REPO_DIR" != "$WORK_ROOT" ]] || die "REPO_DIR must be below WORK_ROOT, not equal to it"

/bin/mkdir -p "$WORK_ROOT"
REPO_PARENT="${REPO_DIR%/*}"
/bin/mkdir -p "$REPO_PARENT"
assert_workspace_paths

if [[ -d "$REPO_DIR/.git" ]]; then
  reject_unsafe_path "repository metadata" "$REPO_DIR/.git"
  assert_clean_checkout
  /usr/bin/git -C "$REPO_DIR" fetch origin
elif [[ -e "$REPO_DIR" ]]; then
  die "REPO_DIR exists without a .git directory: $REPO_DIR"
else
  /usr/local/bin/gh repo clone "$REPO_SLUG" "$REPO_DIR" 2>/dev/null || \
    /opt/homebrew/bin/gh repo clone "$REPO_SLUG" "$REPO_DIR"
fi

DEFAULT_BRANCH="$(/usr/local/bin/gh api "repos/$REPO_SLUG" --jq '.default_branch' 2>/dev/null || \
  /opt/homebrew/bin/gh api "repos/$REPO_SLUG" --jq '.default_branch')"
if ! /usr/bin/git check-ref-format --branch "$DEFAULT_BRANCH" >/dev/null 2>&1; then
  die "GitHub returned an invalid default branch name: $DEFAULT_BRANCH"
fi
assert_workspace_paths
assert_clean_checkout
if /usr/bin/git -C "$REPO_DIR" show-ref --verify --quiet "refs/heads/$DEFAULT_BRANCH"; then
  /usr/bin/git -C "$REPO_DIR" checkout "$DEFAULT_BRANCH"
else
  /usr/bin/git -C "$REPO_DIR" checkout --track -b "$DEFAULT_BRANCH" "origin/$DEFAULT_BRANCH"
fi
/usr/bin/git -C "$REPO_DIR" merge --ff-only "origin/$DEFAULT_BRANCH"
LOCAL_HEAD="$(/usr/bin/git -C "$REPO_DIR" rev-parse HEAD)"
REMOTE_HEAD="$(/usr/bin/git -C "$REPO_DIR" rev-parse "origin/$DEFAULT_BRANCH")"
[[ "$LOCAL_HEAD" == "$REMOTE_HEAD" ]] || die "checkout does not match origin/$DEFAULT_BRANCH; refusing to migrate a divergent branch"
assert_clean_checkout

# Verify output ownership before removing legacy generator inputs.
OWNERSHIP_STATE="$REPO_DIR/.github/ci/.github-actions-generator-state"
CONFIG_PATH="$REPO_DIR/.github-gen/velnor-workflow.toml"
assert_workspace_paths
reject_unsafe_path ".github-gen config" "$CONFIG_PATH"
reject_unsafe_path "ownership state" "$OWNERSHIP_STATE"
verify_static_file_ownership

# Strip static_files blocks after proving their generated outputs are owned.
if [[ -f "$REPO_DIR/.github-gen/velnor-workflow.toml" ]]; then
  /usr/bin/awk '
    /^[[:space:]]*\[\[[[:space:]]*static_files[[:space:]]*\]\][[:space:]]*(#.*)?$/ { skip=1; next }
    skip && /^[[:space:]]*\[[[:space:]]*[^]]+\][[:space:]]*(#.*)?$/ { skip=0 }
    skip && /^[[:space:]]*\[\[[[:space:]]*[^]]+\]\][[:space:]]*(#.*)?$/ { skip=0 }
    skip { next }
    { print }
  ' "$REPO_DIR/.github-gen/velnor-workflow.toml" > "$REPO_DIR/.github-gen/velnor-workflow.toml.tmp"
  /bin/mv "$REPO_DIR/.github-gen/velnor-workflow.toml.tmp" "$REPO_DIR/.github-gen/velnor-workflow.toml"
fi

if [[ "$KEEP_CONFIG" != "--keep-config" ]]; then
  assert_workspace_paths
  reject_unsafe_path ".github-gen/generated-templates" "$REPO_DIR/.github-gen/generated-templates"
  reject_unsafe_path ".github-gen/sources" "$REPO_DIR/.github-gen/sources"
  /bin/rm -rf "$REPO_DIR/.github-gen/generated-templates"
  /bin/rm -rf "$REPO_DIR/.github-gen/sources"
fi

# The S2 generator uses this digest ledger to verify ownership and remove stale
# per-kind ci-unit files whose Velnor header would otherwise exempt them from
# the unowned-workflow scan. Keep the ledger and generated contract intact so
# ownership checks can prove stale deletion; refuse an older state layout
# rather than discard the only ownership proof.
if [[ -e "$OWNERSHIP_STATE" || -L "$OWNERSHIP_STATE" ]] && ! /usr/bin/grep -qx 'schema = 2' "$OWNERSHIP_STATE"; then
  echo "Existing workflow ownership state must use schema 2; refusing to discard stale-output ownership proof" >&2
  exit 1
fi

# Ensure generator revision pin
if [[ ! -f "$REPO_DIR/.github-gen/velnor-workflow.toml" ]]; then
  /bin/mkdir -p "$REPO_DIR/.github-gen"
  /usr/bin/printf '%s\n' \
    "schema = 2" "" \
    "[generator]" \
    "repository = \"$REPO_SLUG\"" \
    "revision = \"$GENERATOR_REV\"" "" \
    "[workflow]" \
    "providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]" \
    "automatic_providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]" \
    "default_dispatch_providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]" \
    "default_branch = \"$DEFAULT_BRANCH\"" \
    "" \
    "[workflow.selectors.github-hosted]" \
    "runs_on = [\"ubuntu-24.04\"]" \
    "" \
    "[workflow.selectors.github-self-hosted]" \
    "runs_on = [\"bastion-scale-set\"]" \
    "" \
    "[workflow.selectors.velnor]" \
    "runs_on = [\"self-hosted\", \"velnor-target-mvp\"]" \
    > "$REPO_DIR/.github-gen/velnor-workflow.toml"
else
  if ! /usr/bin/grep -Eq '^schema = 2$' "$REPO_DIR/.github-gen/velnor-workflow.toml"; then
    echo "Existing .github-gen/velnor-workflow.toml must be converted to schema 2 before clean-room regeneration" >&2
    exit 1
  fi
  if /usr/bin/grep -q '^revision = ' "$REPO_DIR/.github-gen/velnor-workflow.toml"; then
    /usr/bin/sed -i '' "s/^revision = .*/revision = \"$GENERATOR_REV\"/" "$REPO_DIR/.github-gen/velnor-workflow.toml"
  else
    /usr/bin/sed -i '' "/^\[generator\]/a\\
revision = \"$GENERATOR_REV\"
" "$REPO_DIR/.github-gen/velnor-workflow.toml"
  fi
fi

# Regenerate
"$VELNOR_BIN" "$REPO_DIR" --plain --force --default-branch "$DEFAULT_BRANCH"
"$VELNOR_BIN" "$REPO_DIR" --plain --check --default-branch "$DEFAULT_BRANCH"

# The new provider-specific reusable names have a suffix after the kind. A
# generic ci-unit-<kind>.yml still present now is an orphan the ownership
# ledger could not prove safe to delete (for example, because it was lost).
# Stop before staging instead of silently carrying the old workflow forward.
for kind in rust gradle node bun swift opentofu docker homebrew docs; do
  stale="$REPO_DIR/.github/workflows/ci-unit-$kind.yml"
  if [[ -e "$stale" ]]; then
    echo "Unowned obsolete workflow remains after regeneration: $stale; restore valid schema-2 ownership state or remove it after review" >&2
    exit 1
  fi
done

if /usr/bin/git -C "$REPO_DIR" show-ref --verify --quiet "refs/heads/$BRANCH"; then
  die "migration branch already exists locally: $BRANCH"
fi
if ! /usr/bin/git check-ref-format --branch "$BRANCH" >/dev/null 2>&1; then
  die "invalid migration branch name: $BRANCH"
fi
/usr/bin/git -C "$REPO_DIR" switch -c "$BRANCH"
/usr/bin/git -C "$REPO_DIR" add --all -- .github-gen .github
if [[ -e "$REPO_DIR/config/fleet/velnor-host.env" ]] || \
    /usr/bin/git -C "$REPO_DIR" ls-files --error-unmatch -- config/fleet/velnor-host.env >/dev/null 2>&1; then
  /usr/bin/git -C "$REPO_DIR" add --all -- config/fleet/velnor-host.env
fi
if /usr/bin/git -C "$REPO_DIR" diff --cached --quiet; then
  echo "No changes for $REPO_SLUG"
  exit 0
fi

/usr/bin/git -C "$REPO_DIR" commit -s --trailer "Co-authored-by: Codex <codex@openai.com>" -m "$(/usr/bin/printf '%s\n' \
  "chore(ci): clean-room velnor-workflow regeneration" \
  "" \
  "Remove static_files config after verifying schema-2 output ownership;" \
  "regenerate from tailrocks/velnor@${GENERATOR_REV}." \
  "" \
  "Generator-only workflow output.")"

/usr/bin/git -C "$REPO_DIR" push -u origin "$BRANCH"

GH=/usr/local/bin/gh
command -v "$GH" >/dev/null 2>&1 || GH=/opt/homebrew/bin/gh
PR_URL="$("$GH" pr create --repo "$REPO_SLUG" --head "$BRANCH" --base "$DEFAULT_BRANCH" \
  --title "Clean-room velnor-workflow regeneration" \
  --body "Removes legacy \`static_files\` / velnor-actions-generator debt and regenerates from \`tailrocks/velnor@${GENERATOR_REV}\`." \
  2>/dev/null || "$GH" pr list --repo "$REPO_SLUG" --head "$BRANCH" --json url -q '.[0].url')"

echo "PR: $PR_URL"

if [[ "$MERGE" == "--merge" ]]; then
  "$GH" pr merge "$PR_URL" --squash --admin --delete-branch 2>/dev/null || \
    "$GH" pr merge "$PR_URL" --squash --delete-branch
  echo "Merged: $PR_URL"
fi
