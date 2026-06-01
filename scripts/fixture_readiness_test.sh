#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/fixture_readiness.sh"

tmp_dir="$(mktemp -d)"
calls_file="$(mktemp)"
trap 'rm -rf "$tmp_dir" "$calls_file"' EXIT

cat >"$tmp_dir/fixture-status" <<'EOF'
#!/usr/bin/env bash
printf 'fixture-status\n' >>"$VELNOR_FIXTURE_READINESS_TEST_CALLS"
EOF
chmod +x "$tmp_dir/fixture-status"

cat >"$tmp_dir/live-host-doctor" <<'EOF'
#!/usr/bin/env bash
printf 'live-host-doctor\n' >>"$VELNOR_FIXTURE_READINESS_TEST_CALLS"
EOF
chmod +x "$tmp_dir/live-host-doctor"

cat >"$tmp_dir/fixture-audit" <<'EOF'
#!/usr/bin/env bash
printf 'fixture-audit\n' >>"$VELNOR_FIXTURE_READINESS_TEST_CALLS"
EOF
chmod +x "$tmp_dir/fixture-audit"

output="$(
  VELNOR_FIXTURE_READINESS_TEST_CALLS="$calls_file" \
  VELNOR_FIXTURE_STATUS_SCRIPT="$tmp_dir/fixture-status" \
  VELNOR_FIXTURE_AUDIT_SCRIPT="$tmp_dir/fixture-audit" \
  VELNOR_LIVE_HOST_DOCTOR_SCRIPT="$tmp_dir/live-host-doctor" \
  "$SCRIPT"
)"
calls="$(cat "$calls_file")"

if [[ "$output" != *"does not register runners or dispatch workflows"* ]]; then
  echo "fixture readiness did not describe its non-mutating behavior" >&2
  exit 1
fi

if [[ "$calls" != $'fixture-status\nfixture-audit\nlive-host-doctor' ]]; then
  echo "fixture readiness called unexpected scripts: $calls" >&2
  exit 1
fi

: >"$calls_file"
VELNOR_FIXTURE_READINESS_TEST_CALLS="$calls_file" \
VELNOR_FIXTURE_READINESS_CHECK_STATUS=false \
VELNOR_FIXTURE_READINESS_CHECK_AUDIT=false \
VELNOR_FIXTURE_STATUS_SCRIPT="$tmp_dir/fixture-status" \
VELNOR_FIXTURE_AUDIT_SCRIPT="$tmp_dir/fixture-audit" \
VELNOR_LIVE_HOST_DOCTOR_SCRIPT="$tmp_dir/live-host-doctor" \
  "$SCRIPT" >/dev/null
calls="$(cat "$calls_file")"

if [[ "$calls" != "live-host-doctor" ]]; then
  echo "fixture readiness should skip status when requested: $calls" >&2
  exit 1
fi

if VELNOR_FIXTURE_READINESS_CHECK_STATUS=maybe \
  VELNOR_FIXTURE_STATUS_SCRIPT="$tmp_dir/fixture-status" \
  VELNOR_FIXTURE_AUDIT_SCRIPT="$tmp_dir/fixture-audit" \
  VELNOR_LIVE_HOST_DOCTOR_SCRIPT="$tmp_dir/live-host-doctor" \
  "$SCRIPT" >/dev/null 2>&1; then
  echo "fixture readiness should reject invalid boolean controls" >&2
  exit 1
fi

echo "fixture readiness self-test passed"
