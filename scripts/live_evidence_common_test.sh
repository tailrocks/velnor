#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

RUN_ID=123
RUNNER_NAME=velnor-test
JOB_COUNT=1
WORK_DIR="$(mktemp -d)"
LIVE_EVIDENCE_DIR="$(mktemp -d)"
LIVE_EVIDENCE_REPO=owner/repo
LIVE_EVIDENCE_WORKFLOW=ci.yml
LIVE_EVIDENCE_TITLE=Test
LIVE_EVIDENCE_REF=main
LIVE_EVIDENCE_INPUTS=package=a
REQUIRE_DOCKER_SOCKET=true
DUMP_JOB_MESSAGES=
DOCKER_HOST_WORK_DIR=
trap 'rm -rf "$WORK_DIR" "$LIVE_EVIDENCE_DIR"' EXIT

gh() {
  echo "mock gh failure for: $*" >&2
  return 1
}

source "$ROOT/scripts/live_evidence_common.sh"

write_live_evidence "mock-failure"

evidence_file="$LIVE_EVIDENCE_DIR/owner_repo-ci.yml-123.md"

if [[ ! -f "$evidence_file" ]]; then
  echo "expected evidence file $evidence_file" >&2
  exit 1
fi

if ! grep -q "GitHub run snapshot unavailable" "$evidence_file"; then
  echo "evidence file did not record unavailable GitHub run snapshot" >&2
  exit 1
fi

if ! grep -q "mock gh failure" "$evidence_file"; then
  echo "evidence file did not preserve GitHub CLI failure output" >&2
  exit 1
fi

echo "live evidence helper self-test passed"
