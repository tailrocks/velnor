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
if [[ "$1 $2" == "run list" ]]; then
  printf '456\n'
  exit 0
fi
if [[ "$1 $2" == "run view" ]]; then
  printf 'url\thttps://example.test/run/%s\n' "$3"
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

if [[ "$calls" != *"run list --repo donbeave/velnor-actions-fixture --workflow compat.yml --limit 1 --json databaseId --jq .[0].databaseId // \"\""* ]]; then
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

echo "fixture status self-test passed"
