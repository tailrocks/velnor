#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
harness="$script_dir/trusted-harness.sh"
checker="$script_dir/trusted-archive-check.py"
tmp="$(mktemp -d /tmp/velnor-bootstrap-negative.XXXXXX)"
case "$tmp" in
  /tmp/velnor-bootstrap-negative.*) ;;
  *) exit 1 ;;
esac
cleanup() {
  chmod -R u+rwx "$tmp" 2>/dev/null || :
  rm -rf -- "$tmp"
}
trap cleanup EXIT INT TERM

# The hosted wrapper must reject a non-Linux caller before looking for Docker.
set +e
env -i PATH=/usr/bin:/bin G1_HOSTED_CANARY=1 RUNNER_OS=Darwin RUNNER_ARCH=X64 \
  bash "$harness" >"$tmp/host-gate.out" 2>"$tmp/host-gate.err"
host_rc=$?
set -e
test "$host_rc" -ne 0
grep -q 'Linux hosted runner required' "$tmp/host-gate.err"

cat >"$tmp/schema.json" <<'EOF'
{"type":"object","additionalProperties":false,"required":["id"],"properties":{"id":{"type":"integer","minimum":1}}}
EOF
printf '%s\n' '{"id":1,"id":2}' >"$tmp/duplicate.json"
printf '%s\n' '{"id":1.5}' >"$tmp/float.json"
printf '%s\n' '{"id":true}' >"$tmp/boolean.json"
printf '%s\n' '{"id":1,"extra":2}' >"$tmp/extra.json"
printf '%s\n' '{}' >"$tmp/missing.json"
for input in duplicate float boolean extra missing; do
  set +e
  python3 -B "$checker" --validate-handoff "$tmp/schema.json" "$tmp/$input.json" \
    >"$tmp/$input.out" 2>"$tmp/$input.err"
  schema_rc=$?
  set -e
  test "$schema_rc" -ne 0
done
mkdir -- "$tmp/tree"
printf '%s\n' forged >"$tmp/tree/handoff.json"
set +e
python3 -B "$checker" --check-tree "$tmp/tree" >"$tmp/unexpected.json"
unexpected_rc=$?
set -e
test "$unexpected_rc" -eq 2
jq -e '.status == "rejected" and .unexpected == 1 and .read_errors == 0' \
  "$tmp/unexpected.json" >/dev/null

mkdir -- "$tmp/tree/unreadable"
chmod 000 "$tmp/tree/unreadable"
set +e
python3 -B "$checker" --check-tree "$tmp/tree" >"$tmp/unreadable.json"
unreadable_rc=$?
set -e
test "$unreadable_rc" -eq 2
jq -e '.status == "rejected" and .read_errors > 0 and .violations > 0' \
  "$tmp/unreadable.json" >/dev/null

printf '%s\n' accepted >"$tmp/tree/allowed.txt"
python3 -B "$checker" --check-tree "$tmp/tree" --allow-file handoff.json \
  --allow-dir unreadable --allow-file allowed.txt >"$tmp/allowed.json" || :
# The unreadable directory remains a hard rejection even with an allow-list.
jq -e '.status == "rejected" and .read_errors > 0' "$tmp/allowed.json" >/dev/null
