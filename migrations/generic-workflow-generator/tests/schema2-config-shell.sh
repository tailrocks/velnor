#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf -- "$TEMP_ROOT"' EXIT

source "$SCRIPT_DIR/schema2-config.sh"

GENERATOR_ROOT="$TEMP_ROOT/generator"
mkdir -p "$GENERATOR_ROOT"
git -C "$GENERATOR_ROOT" init --quiet
git -C "$GENERATOR_ROOT" config user.name "Migration Test"
git -C "$GENERATOR_ROOT" config user.email "migration-test@example.invalid"
printf 'fixture\n' > "$GENERATOR_ROOT/source.txt"
git -C "$GENERATOR_ROOT" add source.txt
git -C "$GENERATOR_ROOT" commit --quiet -m fixture
GENERATOR_REV="$(git -C "$GENERATOR_ROOT" rev-parse HEAD)"

GENERATOR_BIN="$TEMP_ROOT/velnor-workflow"
cat > "$GENERATOR_BIN" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --revision) printf '%s\n' "${REPORT_REVISION:-unknown}" ;;
  --closure) printf '%s\n' "${REPORT_CLOSURE:-unknown}" ;;
  *)
    target="${1:?target argument is required}"
    case " $* " in
      *" --verify-pinned "*)
        if [[ "${VELNOR_WORKFLOW_PINNED_BINARY:-}" != "$0" ]]; then
          echo "verify-pinned did not receive this exact binary path" >&2
          exit 31
        fi
        if [[ "${VELNOR_WORKFLOW_PINNED_CLOSURE:-}" != "${REPORT_CLOSURE:-}" ]]; then
          echo "verify-pinned did not receive this binary's exact closure" >&2
          exit 32
        fi
        if [[ "$(git -C "$MIGRATION_TARGET_REPO_DIR" status --porcelain --untracked-files=all)" != "" ]]; then
          echo "target repository was written before verify-pinned" >&2
          exit 33
        fi
        if ! grep -Fq "revision = \"${REPORT_REVISION:?}\"" "$target/.github-gen/velnor-workflow.toml"; then
          echo "staged schema-2 config did not contain the exact binary revision" >&2
          exit 34
        fi
        printf '%s\t%s\n' "$VELNOR_WORKFLOW_PINNED_BINARY" "$VELNOR_WORKFLOW_PINNED_CLOSURE" > "$PIN_VERIFY_MARKER"
        if [[ "${FAIL_PIN_VERIFY:-0}" == "1" ]]; then
          echo "fixture pinned verification failure" >&2
          exit 35
        fi
        ;;
      *" --force "*)
        mkdir -p "$target/.github/workflows"
        printf 'name: migrated\n' > "$target/.github/workflows/ci-main.yml"
        ;;
      *" --check "*) ;;
      *) echo "unexpected invocation: $*" >&2; exit 2 ;;
    esac
    ;;
esac
EOF
chmod +x "$GENERATOR_BIN"

if stale_message="$(REPORT_REVISION="$(printf '%040d' 0 | tr '0' 'b')" \
  require_matching_generator_binary "$GENERATOR_REV" "$GENERATOR_ROOT" "$GENERATOR_BIN" 2>&1)"; then
  echo "stale generator was accepted" >&2
  exit 1
fi
if [[ "$stale_message" != *"stale generator"* || "$stale_message" != *"rebuild it from that commit"* ]]; then
  echo "stale generator error is not actionable: $stale_message" >&2
  exit 1
fi

if ! matching_closure="$(REPORT_REVISION="$GENERATOR_REV" REPORT_CLOSURE="$(printf 'a%.0s' {1..64})" \
  require_matching_generator_binary "$GENERATOR_REV" "$GENERATOR_ROOT" "$GENERATOR_BIN")"; then
  echo "matching generator was rejected" >&2
  exit 1
fi
if [[ "$matching_closure" != "$(printf 'a%.0s' {1..64})" ]]; then
  echo "generator closure was not returned exactly" >&2
  exit 1
fi

touch "$GENERATOR_ROOT/untracked-change"
if dirty_message="$(REPORT_REVISION="$GENERATOR_REV" REPORT_CLOSURE="$(printf 'a%.0s' {1..64})" \
  require_matching_generator_binary "$GENERATOR_REV" "$GENERATOR_ROOT" "$GENERATOR_BIN" 2>&1)"; then
  echo "dirty generator checkout was accepted" >&2
  exit 1
fi
if [[ "$dirty_message" != *"has tracked or untracked changes"* ]]; then
  echo "dirty checkout error is not actionable: $dirty_message" >&2
  exit 1
fi
rm -f "$GENERATOR_ROOT/untracked-change"

# A stale configurable binary must fail before repo clone/API calls or work-root
# creation. This guards the ordering in migrate-repo.sh, not just the helper.
GH_BIN="$TEMP_ROOT/bin"
mkdir -p "$GH_BIN"
cat > "$GH_BIN/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
touch "$GH_CALLED_MARKER"
exit 99
EOF
chmod +x "$GH_BIN/gh"
WORK_ROOT="$TEMP_ROOT/work"
GH_CALLED_MARKER="$TEMP_ROOT/gh-was-called"
if migration_message="$(
  PATH="$GH_BIN:$PATH" \
  VELNOR_ROOT="$GENERATOR_ROOT" \
  VELNOR_BIN="$GENERATOR_BIN" \
  WORK_ROOT="$WORK_ROOT" \
  GH_CALLED_MARKER="$GH_CALLED_MARKER" \
  REPORT_REVISION="$(printf '%040d' 0 | tr '0' 'b')" \
  REPORT_CLOSURE="$(printf 'a%.0s' {1..64})" \
    "$SCRIPT_DIR/migrate-repo.sh" example/repo 2>&1
)"; then
  echo "migration accepted a stale binary" >&2
  exit 1
fi
if [[ "$migration_message" != *"stale generator"* ]]; then
  echo "migration stale-binary error is not actionable: $migration_message" >&2
  exit 1
fi
if [[ -e "$GH_CALLED_MARKER" || -e "$WORK_ROOT" ]]; then
  echo "migration touched the target workspace before generator pin validation" >&2
  exit 1
fi

# Exercise the positive staging path. A deliberately failing pin verification
# must leave the cloned target checkout clean; a later passing verification
# must carry the exact binary and closure into the verifier before syncing.
MIGRATION_GH_BIN="$TEMP_ROOT/migration-bin"
mkdir -p "$MIGRATION_GH_BIN"
cat > "$MIGRATION_GH_BIN/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-} ${2:-}" in
  "repo clone")
    git clone --quiet "$TEST_REMOTE" "$4"
    ;;
  "api repos/example/repo") printf 'main\n' ;;
  "pr create") printf 'https://example.invalid/pull/1\n' ;;
  "pr list") printf '[]\n' ;;
  *) echo "unexpected gh invocation: $*" >&2; exit 90 ;;
esac
EOF
chmod +x "$MIGRATION_GH_BIN/gh"

TEST_REMOTE="$TEMP_ROOT/repo.git"
SEED_REPO="$TEMP_ROOT/repo-seed"
git init --bare --quiet --initial-branch=main "$TEST_REMOTE"
git init --quiet --initial-branch=main "$SEED_REPO"
git -C "$SEED_REPO" config user.name "Migration Test"
git -C "$SEED_REPO" config user.email "migration-test@example.invalid"
mkdir -p "$SEED_REPO/.github-gen"
cat > "$SEED_REPO/.github-gen/velnor-workflow.toml" <<'EOF'
schema = 1

[generator]
repository = "example/repo"

[workflow]
runners = "both"

[[check_profile]]
id = "local-smoke"
tasks = ["check-smoke"]
runner = "velnor"
EOF
git -C "$SEED_REPO" add .github-gen/velnor-workflow.toml
git -C "$SEED_REPO" commit --quiet -m fixture
git -C "$SEED_REPO" remote add origin "$TEST_REMOTE"
git -C "$SEED_REPO" push --quiet -u origin main

FAIL_WORK_ROOT="$TEMP_ROOT/fail-work"
FAIL_TARGET="$FAIL_WORK_ROOT/example/repo"
FAIL_VERIFY_MARKER="$TEMP_ROOT/fail-verify.marker"
if migration_message="$(
  PATH="$MIGRATION_GH_BIN:$PATH" \
  TEST_REMOTE="$TEST_REMOTE" \
  VELNOR_ROOT="$GENERATOR_ROOT" \
  VELNOR_BIN="$GENERATOR_BIN" \
  WORK_ROOT="$FAIL_WORK_ROOT" \
  REPORT_REVISION="$GENERATOR_REV" \
  REPORT_CLOSURE="$(printf 'a%.0s' {1..64})" \
  MIGRATION_TARGET_REPO_DIR="$FAIL_TARGET" \
  PIN_VERIFY_MARKER="$FAIL_VERIFY_MARKER" \
  FAIL_PIN_VERIFY=1 \
  GIT_AUTHOR_NAME="Migration Test" \
  GIT_AUTHOR_EMAIL="migration-test@example.invalid" \
  GIT_COMMITTER_NAME="Migration Test" \
  GIT_COMMITTER_EMAIL="migration-test@example.invalid" \
    "$SCRIPT_DIR/migrate-repo.sh" example/repo 2>&1
)"; then
  echo "migration accepted a failing --verify-pinned result" >&2
  exit 1
fi
if [[ "$migration_message" != *"fixture pinned verification failure"* ]]; then
  echo "migration did not run --verify-pinned with the staged tree: $migration_message" >&2
  exit 1
fi
if [[ ! -f "$FAIL_VERIFY_MARKER" ]]; then
  echo "--verify-pinned did not record the exact pin environment" >&2
  exit 1
fi
if [[ "$(cat "$FAIL_VERIFY_MARKER")" != "$GENERATOR_BIN$(printf '\t')$(printf 'a%.0s' {1..64})" ]]; then
  echo "--verify-pinned received the wrong binary or closure: $(cat "$FAIL_VERIFY_MARKER")" >&2
  exit 1
fi
if [[ -n "$(git -C "$FAIL_TARGET" status --porcelain --untracked-files=all)" || -e "$FAIL_TARGET/.github/workflows/ci-main.yml" ]]; then
  echo "failed --verify-pinned changed the target repository" >&2
  exit 1
fi

PASS_WORK_ROOT="$TEMP_ROOT/pass-work"
PASS_TARGET="$PASS_WORK_ROOT/example/repo"
PASS_VERIFY_MARKER="$TEMP_ROOT/pass-verify.marker"
PATH="$MIGRATION_GH_BIN:$PATH" \
TEST_REMOTE="$TEST_REMOTE" \
VELNOR_ROOT="$GENERATOR_ROOT" \
VELNOR_BIN="$GENERATOR_BIN" \
WORK_ROOT="$PASS_WORK_ROOT" \
REPORT_REVISION="$GENERATOR_REV" \
REPORT_CLOSURE="$(printf 'a%.0s' {1..64})" \
MIGRATION_TARGET_REPO_DIR="$PASS_TARGET" \
PIN_VERIFY_MARKER="$PASS_VERIFY_MARKER" \
GIT_AUTHOR_NAME="Migration Test" \
GIT_AUTHOR_EMAIL="migration-test@example.invalid" \
GIT_COMMITTER_NAME="Migration Test" \
GIT_COMMITTER_EMAIL="migration-test@example.invalid" \
  "$SCRIPT_DIR/migrate-repo.sh" example/repo >/dev/null
if [[ "$(cat "$PASS_VERIFY_MARKER")" != "$GENERATOR_BIN$(printf '\t')$(printf 'a%.0s' {1..64})" ]]; then
  echo "passing --verify-pinned received the wrong binary or closure" >&2
  exit 1
fi
if [[ ! -f "$PASS_TARGET/.github/workflows/ci-main.yml" ]]; then
  echo "verified staged output was not synced to the target repository" >&2
  exit 1
fi
if ! grep -Fq 'schema = 2' "$PASS_TARGET/.github-gen/velnor-workflow.toml"; then
  echo "verified schema-2 config was not synced to the target repository" >&2
  exit 1
fi
MIGRATION_COMMIT_MESSAGE="$(git -C "$PASS_TARGET" log -1 --format=%B)"
if [[ "$MIGRATION_COMMIT_MESSAGE" != *"Signed-off-by: Migration Test <migration-test@example.invalid>"* || \
      "$MIGRATION_COMMIT_MESSAGE" != *"Co-authored-by: Codex <codex@openai.com>"* ]]; then
  echo "migration commit omitted the required sign-off or co-author trailer" >&2
  exit 1
fi

echo "schema2-config shell checks passed"
