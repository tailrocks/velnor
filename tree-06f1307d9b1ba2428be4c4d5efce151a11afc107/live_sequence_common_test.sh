#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/live_sequence_common.sh"

assert_passes() {
  local label="$1"
  shift

  if ! "$@"; then
    echo "$label: expected success" >&2
    exit 1
  fi
}

assert_fails() {
  local label="$1"
  shift

  if "$@" >/dev/null 2>&1; then
    echo "$label: expected failure" >&2
    exit 1
  fi
}

assert_passes "true bool" velnor_require_bool TEST_BOOL true
assert_passes "false bool" velnor_require_bool TEST_BOOL false
assert_fails "mixed-case bool" velnor_require_bool TEST_BOOL True
assert_fails "empty bool" velnor_require_bool TEST_BOOL ""
assert_fails "word bool" velnor_require_bool TEST_BOOL yes

assert_passes "one job" velnor_require_positive_int TEST_COUNT 1
assert_passes "many jobs" velnor_require_positive_int TEST_COUNT 42
assert_fails "zero jobs" velnor_require_positive_int TEST_COUNT 0
assert_fails "negative jobs" velnor_require_positive_int TEST_COUNT -1
assert_fails "non-number jobs" velnor_require_positive_int TEST_COUNT two
assert_passes "empty optional count" velnor_require_optional_positive_int TEST_COUNT ""
assert_passes "valid optional count" velnor_require_optional_positive_int TEST_COUNT 123
assert_fails "invalid optional count" velnor_require_optional_positive_int TEST_COUNT abc
assert_passes "nonempty value" velnor_require_nonempty TEST_VALUE value
assert_fails "empty value" velnor_require_nonempty TEST_VALUE ""
assert_passes "repo slug" velnor_require_repo_slug TEST_REPO owner/repo-name
assert_passes "repo slug with dots" velnor_require_repo_slug TEST_REPO org.name/repo.name
assert_fails "missing repo owner" velnor_require_repo_slug TEST_REPO repo
assert_fails "bad repo slug" velnor_require_repo_slug TEST_REPO owner/repo/extra
assert_passes "empty optional workflow" velnor_require_optional_workflow_file TEST_WORKFLOW ""
assert_passes "yml workflow" velnor_require_optional_workflow_file TEST_WORKFLOW ci.yml
assert_passes "yaml workflow" velnor_require_optional_workflow_file TEST_WORKFLOW release.yaml
assert_fails "workflow path" velnor_require_optional_workflow_file TEST_WORKFLOW .github/workflows/ci.yml
assert_fails "workflow extension" velnor_require_optional_workflow_file TEST_WORKFLOW ci.txt
assert_passes "required workflow" velnor_require_workflow_file TEST_WORKFLOW compat.yml
assert_fails "empty required workflow" velnor_require_workflow_file TEST_WORKFLOW ""
assert_fails "bad required workflow" velnor_require_workflow_file TEST_WORKFLOW compat.txt

assert_passes "default evidence controls" velnor_require_live_evidence_controls

VELNOR_LIVE_EVIDENCE_LOG_LINES=12
VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES=5
assert_passes "explicit evidence controls" velnor_require_live_evidence_controls
unset VELNOR_LIVE_EVIDENCE_LOG_LINES VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES

VELNOR_LIVE_EVIDENCE_LOG_LINES=0
assert_fails "zero log lines" velnor_require_live_evidence_controls
unset VELNOR_LIVE_EVIDENCE_LOG_LINES

VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES=abc
assert_fails "bad local entries" velnor_require_live_evidence_controls
unset VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES

gh() {
  if [[ "$1 $2" == "api repos/owner/repo/actions/runners" ]]; then
    printf 'velnor-target-mvp\tonline\tself-hosted,velnor-target-mvp\n'
    printf 'other-runner\tonline\tself-hosted,velnor-target-mvp\n'
    printf 'offline-runner\toffline\tself-hosted,velnor-target-mvp\n'
    return 0
  fi
  echo "unexpected gh call: $*" >&2
  return 2
}

assert_fails "matching online runner" \
  velnor_fail_if_other_online_runners_match_labels owner/repo velnor-target-mvp velnor-target-mvp

assert_passes "nonmatching label" \
  velnor_fail_if_other_online_runners_match_labels owner/repo velnor-target-mvp hetzner-sentry-ci

VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true
assert_passes "matching runner override" \
  velnor_fail_if_other_online_runners_match_labels owner/repo velnor-target-mvp velnor-target-mvp
unset VELNOR_ALLOW_OTHER_MATCHING_RUNNERS

job_model="$(velnor_print_job_execution_model 3 "Target")"
if [[ "$job_model" != *"one daemon with multiple internal GitHub runner slots"* ]]; then
  echo "job execution model output did not describe daemon slot behavior" >&2
  exit 1
fi
if [[ "$job_model" != *"can run concurrently"* ]]; then
  echo "job execution model output did not describe concurrent job containers" >&2
  exit 1
fi
if [[ "$job_model" != *"3 job(s) through repeated --once"* ]]; then
  echo "job execution model output did not describe current smoke execution" >&2
  exit 1
fi

echo "live sequence helper self-test passed"
