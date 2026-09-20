#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
C1_DIR="$(cd -- "$TEST_DIR/.." && pwd)"

[[ -x "$C1_DIR/apt-plan-guard.sh" ]] || {
  printf 'APT transaction guard must be executable in the deployed tree\n' >&2
  exit 1
}

for file in \
  "$C1_DIR/provision-bastion.sh" \
  "$C1_DIR/apt-plan-guard.sh" \
  "$C1_DIR/deploy-bastion.sh" \
  "$C1_DIR/promote-staged-tree.sh" \
  "$TEST_DIR/run.sh" \
  "$TEST_DIR/fake-host.sh"; do
  bash -n "$file"
done

command -v shellcheck >/dev/null 2>&1 || {
  printf 'shellcheck is required for this validation script\n' >&2
  exit 1
}
shellcheck -s bash -S warning \
  "$C1_DIR/provision-bastion.sh" \
  "$C1_DIR/apt-plan-guard.sh" \
  "$C1_DIR/deploy-bastion.sh" \
  "$C1_DIR/promote-staged-tree.sh" \
  "$TEST_DIR/run.sh"
# fake-host sources provision-bastion.sh and supplies its globals to the
# provisioner's functions; standalone ShellCheck cannot see those reads.
shellcheck -s bash -S warning -e SC2034 "$TEST_DIR/fake-host.sh"

if grep -Eq 'curl[^[:cntrl:]]*\|[[:space:]]*sh|StrictHostKeyChecking=accept-new' \
  "$C1_DIR/provision-bastion.sh" "$C1_DIR/deploy-bastion.sh" \
  "$C1_DIR/promote-staged-tree.sh" "$C1_DIR/targets.env"; then
  printf 'unpinned shell installer or SSH TOFU option found\n' >&2
  exit 1
fi
grep -Fq 'StrictHostKeyChecking=yes' "$C1_DIR/README.md"
grep -Fq "UserKnownHostsFile=\$KNOWN_HOSTS" "$C1_DIR/README.md"
grep -Fq 'GlobalKnownHostsFile=/dev/null' "$C1_DIR/README.md"
grep -Fq 'regular, non-symlink, root-owned file before changing its mode' "$C1_DIR/README.md"
grep -Fq 'chmod 0600 -- <exact-database-path>' "$C1_DIR/README.md"
grep -Fq 'Use literal configured paths, never a glob' "$C1_DIR/README.md"
grep -Fq 'tar --no-same-owner --no-same-permissions' "$C1_DIR/promote-staged-tree.sh"
grep -Fq 'chown -R 0:0 -- "$tree_root"' "$C1_DIR/promote-staged-tree.sh"
grep -Fq 'find "$tree_root" -type d -exec chmod 0755 -- {} +' "$C1_DIR/promote-staged-tree.sh"
grep -Fq 'find "$tree_root" -type f -perm /111 -exec chmod 0755 -- {} +' "$C1_DIR/promote-staged-tree.sh"
grep -Fq 'find "$tree_root" -type f ! -perm /111 -exec chmod 0644 -- {} +' "$C1_DIR/promote-staged-tree.sh"

python3 - "$C1_DIR/merge-daemon-json.py" "$C1_DIR/apt-input-fingerprint.py" \
  "$C1_DIR/provision-permit-ledger-roster.py" \
  "$TEST_DIR/fake-daemon-json.py" "$TEST_DIR/test_permit_ledger_roster.py" \
  "$TEST_DIR/test_apt_input_fingerprint.py" <<'PY'
from pathlib import Path
import sys

for filename in sys.argv[1:]:
    source = Path(filename).read_text(encoding="utf-8")
    compile(source, filename, "exec")
PY

bash "$TEST_DIR/fake-host.sh"
python3 "$TEST_DIR/fake-daemon-json.py"
python3 "$TEST_DIR/test_permit_ledger_roster.py"
python3 "$TEST_DIR/test_apt_input_fingerprint.py"
printf 'C1 authoring checks passed; no host or APT commands were run.\n'
