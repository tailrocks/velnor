#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
C1_DIR="$(cd -- "$TEST_DIR/.." && pwd)"

for file in \
  "$C1_DIR/provision-bastion.sh" \
  "$TEST_DIR/run.sh" \
  "$TEST_DIR/fake-host.sh"; do
  bash -n "$file"
done

command -v shellcheck >/dev/null 2>&1 || {
  printf 'shellcheck is required for this validation script\n' >&2
  exit 1
}
shellcheck -s bash \
  "$C1_DIR/provision-bastion.sh" \
  "$TEST_DIR/run.sh" \
  "$TEST_DIR/fake-host.sh"

if grep -Eq 'curl[^[:cntrl:]]*\|[[:space:]]*sh|StrictHostKeyChecking=accept-new' \
  "$C1_DIR/provision-bastion.sh" "$C1_DIR/targets.env"; then
  printf 'unpinned shell installer or SSH TOFU option found\n' >&2
  exit 1
fi
grep -Fq 'StrictHostKeyChecking=yes' "$C1_DIR/README.md"
grep -Fq "UserKnownHostsFile=\$KNOWN_HOSTS" "$C1_DIR/README.md"
grep -Fq 'GlobalKnownHostsFile=/dev/null' "$C1_DIR/README.md"

python3 - "$C1_DIR/merge-daemon-json.py" "$TEST_DIR/fake-daemon-json.py" <<'PY'
from pathlib import Path
import sys

for filename in sys.argv[1:]:
    source = Path(filename).read_text(encoding="utf-8")
    compile(source, filename, "exec")
PY

bash "$TEST_DIR/fake-host.sh"
python3 "$TEST_DIR/fake-daemon-json.py"
printf 'C1 authoring checks passed; no host or APT commands were run.\n'
