#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/fixture_report.sh"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

cat >"$tmp_dir/status" <<'EOF'
#!/usr/bin/env bash
echo "status ok"
EOF
chmod +x "$tmp_dir/status"

cat >"$tmp_dir/audit" <<'EOF'
#!/usr/bin/env bash
echo "audit ok"
EOF
chmod +x "$tmp_dir/audit"

cat >"$tmp_dir/doctor" <<'EOF'
#!/usr/bin/env bash
echo "doctor ok"
EOF
chmod +x "$tmp_dir/doctor"

report="$tmp_dir/report.md"
VELNOR_FIXTURE_REPORT_PATH="$report" \
VELNOR_FIXTURE_STATUS_SCRIPT="$tmp_dir/status" \
VELNOR_FIXTURE_AUDIT_SCRIPT="$tmp_dir/audit" \
VELNOR_LIVE_HOST_DOCTOR_SCRIPT="$tmp_dir/doctor" \
  "$SCRIPT" >/dev/null

if [[ ! -s "$report" ]]; then
  echo "fixture report was not written" >&2
  exit 1
fi

expected_values=(
  "Fixture Workflow Status"
  "Fixture Feature Audit"
  "Live Host Readiness"
  "Next Action"
  "velnor_commit:"
  "velnor_branch:"
  "velnor_dirty_files:"
  "velnor_work_dir:"
  "docker_host:"
  "docker_host_work_dir:"
  "require_docker_socket:"
  "status ok"
  "audit ok"
  "doctor ok"
  'Run `scripts/fixture_smoke.sh`'
)

for expected in "${expected_values[@]}"; do
  if ! grep -q "$expected" "$report"; then
    echo "fixture report missing expected content: $expected" >&2
    exit 1
  fi
done

cat >"$tmp_dir/doctor" <<'EOF'
#!/usr/bin/env bash
echo "doctor failed"
exit 42
EOF
chmod +x "$tmp_dir/doctor"

if VELNOR_FIXTURE_REPORT_PATH="$report" \
  VELNOR_FIXTURE_STATUS_SCRIPT="$tmp_dir/status" \
  VELNOR_FIXTURE_AUDIT_SCRIPT="$tmp_dir/audit" \
  VELNOR_LIVE_HOST_DOCTOR_SCRIPT="$tmp_dir/doctor" \
  "$SCRIPT" >/dev/null 2>&1; then
  echo "fixture report should fail when a section fails" >&2
  exit 1
fi

if ! grep -q "doctor failed" "$report"; then
  echo "fixture report did not retain failing section output" >&2
  exit 1
fi

if ! grep -q -- "- status: 42" "$report"; then
  echo "fixture report did not record failing section status" >&2
  exit 1
fi

if ! grep -q "Fix the failing section above" "$report"; then
  echo "fixture report did not include failure next action" >&2
  exit 1
fi

echo "fixture report self-test passed"
