#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/fixture_status.sh"

mock_bin="$(mktemp -d)"
calls_file="$(mktemp)"
trap 'rm -rf "$mock_bin" "$calls_file"' EXIT

cat >"$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$VELNOR_FIXTURE_STATUS_TEST_CALLS"
if [[ "$1" == "--version" ]]; then
  printf 'gh version 0.0.0-test\n'
  exit 0
fi
if [[ "$1 $2" == "run list" ]]; then
  printf '[{"databaseId":456}]\n'
  exit 0
fi
if [[ "$1 $2" == "run view" ]]; then
  case "${VELNOR_FIXTURE_STATUS_CASE:-success}" in
    success)
      printf '{"url":"https://example.test/run/%s","status":"completed","conclusion":"success","jobs":[{"name":"compat","status":"completed","conclusion":"success"}]}\n' "$3"
      ;;
    failed)
      printf '{"url":"https://example.test/run/%s","status":"completed","conclusion":"failure","jobs":[{"name":"compat","status":"completed","conclusion":"failure"}]}\n' "$3"
      ;;
    cancelled)
      printf '{"url":"https://example.test/run/%s","status":"completed","conclusion":"cancelled","jobs":[{"name":"compat","status":"completed","conclusion":"cancelled"}]}\n' "$3"
      ;;
    in-progress)
      printf '{"url":"https://example.test/run/%s","status":"in_progress","conclusion":null,"jobs":[{"name":"compat","status":"in_progress","conclusion":null}]}\n' "$3"
      ;;
    *)
      echo "unknown fixture status test case: ${VELNOR_FIXTURE_STATUS_CASE}" >&2
      exit 2
      ;;
  esac
  exit 0
fi
echo "unexpected gh call: $*" >&2
exit 2
EOF
chmod +x "$mock_bin/gh"

output="$(PATH="$mock_bin:$PATH" VELNOR_FIXTURE_STATUS_TEST_CALLS="$calls_file" "$SCRIPT")"
calls="$(cat "$calls_file")"

if [[ "$output" != *"https://example.test/run/456"* ]]; then
  echo "fixture status did not view latest run id: $output" >&2
  exit 1
fi

if [[ "$calls" != *"run list --repo tailrocks/velnor-actions-fixture --workflow compat.yml --limit 1 --json databaseId"* ]]; then
  echo "fixture status did not query latest compat run: $calls" >&2
  exit 1
fi

: >"$calls_file"
output="$(PATH="$mock_bin:$PATH" VELNOR_FIXTURE_STATUS_TEST_CALLS="$calls_file" VELNOR_FIXTURE_RUN_ID=789 "$SCRIPT")"
calls="$(cat "$calls_file")"

if [[ "$output" != *"https://example.test/run/789"* ]]; then
  echo "fixture status did not view explicit run id: $output" >&2
  exit 1
fi

if [[ "$calls" == *"run list"* ]]; then
  echo "fixture status should not list runs when explicit run id is set: $calls" >&2
  exit 1
fi

if PATH="$mock_bin:$PATH" VELNOR_FIXTURE_STATUS_TEST_CALLS="$calls_file" VELNOR_FIXTURE_RUN_ID=abc "$SCRIPT" >/dev/null 2>&1; then
  echo "fixture status should reject non-numeric explicit run id" >&2
  exit 1
fi

for status_case in failed cancelled in-progress; do
  if PATH="$mock_bin:$PATH" VELNOR_FIXTURE_STATUS_TEST_CALLS="$calls_file" VELNOR_FIXTURE_STATUS_CASE="$status_case" VELNOR_FIXTURE_RUN_ID=789 "$SCRIPT" >/dev/null 2>&1; then
    echo "fixture status should reject ${status_case} workflow runs" >&2
    exit 1
  fi
done

echo "fixture status self-test passed"
